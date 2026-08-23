//! Loss-aware Markdown translation at the Obsidian/Outline boundary.
//!
//! Pure syntax translations live here so ZIP export, direct publication, and
//! future pull routes use the same rules. File materialization (notably remote
//! attachments) remains the responsibility of the surrounding workflow.

use regex::{Captures, Regex};
use std::sync::LazyLock;

static OBSIDIAN_CALLOUT_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<indent>[ \t]*)> ?\[!(?P<kind>[A-Za-z0-9_-]+)\][+-]?[ \t]*(?P<title>.*)$")
        .expect("Obsidian callout regex should compile")
});
static OUTLINE_CALLOUT_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<indent>[ \t]*):::(?P<kind>info|tip|success|warning)[ \t]*$")
        .expect("Outline callout regex should compile")
});
static MARKDOWN_LINK_DESTINATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\]\((?P<destination>[^\s)]+)\)")
        .expect("Markdown link destination regex should compile")
});
static OUTLINE_DOCUMENT_DESTINATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:https?://[^/]+)?/doc/(?P<id>[^/?#]+)(?P<suffix>[?#].*)?$")
        .expect("Outline document destination regex should compile")
});
static MARKDOWN_DOCUMENT_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(?P<label>[^\]]+)\]\((?P<destination>[^\s)]+)\)")
        .expect("Markdown document link regex should compile")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutlineMarkdownOptions {
    /// Remove Obsidian-generated lists whose items only target headings.
    pub remove_toc: bool,
}

/// Translate vault Markdown into the syntax accepted by Outline.
///
/// YAML frontmatter is intentionally not published. Vulcan keeps remote
/// identity in durable mapping state, so no Outline bookkeeping needs to leak
/// into either the vault or the remote document body.
#[must_use]
pub fn obsidian_to_outline_markdown(source: &str, options: OutlineMarkdownOptions) -> String {
    let without_frontmatter = strip_frontmatter(source);
    let without_toc = if options.remove_toc {
        remove_obsidian_toc(&without_frontmatter)
    } else {
        without_frontmatter
    };
    convert_obsidian_callouts(&without_toc)
}

/// Translate reversible Outline-specific syntax into canonical Obsidian
/// Markdown. Frontmatter and removed TOCs cannot be reconstructed.
#[must_use]
pub fn outline_to_obsidian_markdown(source: &str) -> String {
    convert_outline_callouts(source)
}

/// Rewrite Markdown link destinations with a caller-provided mapping.
///
/// This is used outbound after hierarchy planning (relative archive paths to
/// durable Outline document URLs) and inbound after binding resolution
/// (Outline document URLs to local vault targets).
#[must_use]
pub fn rewrite_markdown_link_destinations(
    source: &str,
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> String {
    MARKDOWN_LINK_DESTINATION
        .replace_all(source, |captures: &Captures<'_>| {
            let destination = &captures["destination"];
            resolve(destination).map_or_else(
                || captures[0].to_string(),
                |replacement| format!("]({replacement})"),
            )
        })
        .into_owned()
}

/// Turn Outline `/doc/<id>` links into Obsidian wikilinks when the remote ID
/// has a known local binding. Unknown links remain valid Markdown links.
#[must_use]
pub fn outline_document_links_to_obsidian(
    source: &str,
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> String {
    MARKDOWN_DOCUMENT_LINK
        .replace_all(source, |captures: &Captures<'_>| {
            let destination = &captures["destination"];
            let Some(destination_captures) = OUTLINE_DOCUMENT_DESTINATION.captures(destination)
            else {
                return captures[0].to_string();
            };
            let Some(local_target) = resolve(&destination_captures["id"]) else {
                return captures[0].to_string();
            };
            let local_target = local_target.strip_suffix(".md").unwrap_or(&local_target);
            let suffix = destination_captures
                .name("suffix")
                .map_or("", |value| value.as_str());
            let label = &captures["label"];
            let target = format!("{local_target}{suffix}");
            let default_label = local_target.rsplit('/').next().unwrap_or(local_target);
            if suffix.is_empty() && label == default_label {
                format!("[[{target}]]")
            } else {
                format!("[[{target}|{label}]]")
            }
        })
        .into_owned()
}

#[must_use]
pub fn outline_document_url(remote_id: &str) -> String {
    format!("/doc/{remote_id}")
}

fn strip_frontmatter(source: &str) -> String {
    let mut offset = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let Some((first, next)) = next_line(source, offset) else {
        return source.to_string();
    };
    if first.trim_end_matches('\r') != "---" {
        return source.to_string();
    }
    offset = next;
    while let Some((line, next)) = next_line(source, offset) {
        if matches!(line.trim_end_matches('\r'), "---" | "...") {
            return source[next..].to_string();
        }
        offset = next;
    }
    source.to_string()
}

fn remove_obsidian_toc(source: &str) -> String {
    let lines = source.split('\n').collect::<Vec<_>>();
    let mut result = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if !is_toc_line(lines[index]) {
            result.push(lines[index]);
            index += 1;
            continue;
        }
        while index < lines.len() && is_toc_line(lines[index]) {
            index += 1;
        }
        if index < lines.len() && lines[index].trim().is_empty() {
            index += 1;
        }
        if !result.is_empty()
            && !result.last().is_some_and(|line| line.trim().is_empty())
            && index < lines.len()
            && !lines[index].trim().is_empty()
        {
            result.push("");
        }
    }
    result.join("\n")
}

fn is_toc_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let after_marker = ['-', '*', '+']
        .iter()
        .find_map(|marker| trimmed.strip_prefix(*marker).and_then(strip_required_space))
        .or_else(|| strip_numbered_list_marker(trimmed));
    after_marker.is_some_and(|item| {
        let item = item.trim();
        (item.contains("[[#") && item.ends_with("]]"))
            || (item.contains("](#") && item.ends_with(')'))
    })
}

fn strip_required_space(value: &str) -> Option<&str> {
    let stripped = value.trim_start_matches([' ', '\t']);
    (stripped.len() < value.len()).then_some(stripped)
}

fn strip_numbered_list_marker(value: &str) -> Option<&str> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = value.get(digits..)?;
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    strip_required_space(rest)
}

fn convert_obsidian_callouts(source: &str) -> String {
    let lines = source.split('\n').collect::<Vec<_>>();
    let mut result = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut code_fence = None::<char>;
    while index < lines.len() {
        let line = lines[index];
        update_code_fence(line, &mut code_fence);
        if code_fence.is_some() {
            result.push(line.to_string());
            index += 1;
            continue;
        }
        let Some(captures) = OBSIDIAN_CALLOUT_OPEN.captures(line) else {
            result.push(line.to_string());
            index += 1;
            continue;
        };
        let indent = &captures["indent"];
        let kind = outline_callout_kind(&captures["kind"]);
        result.push(format!("{indent}:::{kind}"));
        let title = captures["title"].trim();
        if !title.is_empty() {
            result.push(format!("{indent}{title}"));
        }
        index += 1;
        while index < lines.len() {
            let continuation = lines[index];
            let Some(rest) = continuation.strip_prefix(indent) else {
                break;
            };
            let Some(rest) = rest.strip_prefix('>') else {
                break;
            };
            result.push(format!(
                "{indent}{}",
                rest.strip_prefix(' ').unwrap_or(rest)
            ));
            index += 1;
        }
        if !result.last().is_some_and(String::is_empty) {
            result.push(String::new());
        }
        result.push(format!("{indent}:::"));
    }
    result.join("\n")
}

fn convert_outline_callouts(source: &str) -> String {
    let lines = source.split('\n').collect::<Vec<_>>();
    let mut result = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut code_fence = None::<char>;
    while index < lines.len() {
        let line = lines[index];
        update_code_fence(line, &mut code_fence);
        if code_fence.is_some() {
            result.push(line.to_string());
            index += 1;
            continue;
        }
        let Some(captures) = OUTLINE_CALLOUT_OPEN.captures(line) else {
            result.push(line.to_string());
            index += 1;
            continue;
        };
        let indent = &captures["indent"];
        result.push(format!("{indent}> [!{}]", &captures["kind"]));
        index += 1;
        while index < lines.len() {
            let body = lines[index];
            if body == format!("{indent}:::") {
                index += 1;
                break;
            }
            let body = body.strip_prefix(indent).unwrap_or(body);
            if body.is_empty() {
                result.push(format!("{indent}>"));
            } else {
                result.push(format!("{indent}> {body}"));
            }
            index += 1;
        }
    }
    result.join("\n")
}

fn outline_callout_kind(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "tip" | "example" => "tip",
        "success" => "success",
        "warning" | "failure" | "danger" | "bug" => "warning",
        _ => "info",
    }
}

fn update_code_fence(line: &str, code_fence: &mut Option<char>) {
    let trimmed = line.trim_start();
    let marker = if trimmed.starts_with("```") {
        Some('`')
    } else if trimmed.starts_with("~~~") {
        Some('~')
    } else {
        None
    };
    if let Some(marker) = marker {
        if *code_fence == Some(marker) {
            *code_fence = None;
        } else if code_fence.is_none() {
            *code_fence = Some(marker);
        }
    }
}

fn next_line(source: &str, start: usize) -> Option<(&str, usize)> {
    if start >= source.len() {
        return None;
    }
    let rest = source.get(start..)?;
    let end = rest
        .find('\n')
        .map_or(source.len(), |offset| start + offset);
    let next = if end < source.len() { end + 1 } else { end };
    Some((&source[start..end], next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_strips_frontmatter_and_converts_callouts() {
        let source = "---\ntags: [private]\n---\n# Note\n\n> [!DANGER]- Stop\n> First line\n>\n> Second line\n";
        let converted = obsidian_to_outline_markdown(source, OutlineMarkdownOptions::default());

        assert_eq!(
            converted,
            "# Note\n\n:::warning\nStop\nFirst line\n\nSecond line\n\n:::\n"
        );
    }

    #[test]
    fn outbound_preserves_callout_like_text_in_code_fences() {
        let source = "```md\n> [!WARNING] literal\n```\n\n> [!TIP] Real\n> body";
        let converted = obsidian_to_outline_markdown(source, OutlineMarkdownOptions::default());

        assert!(converted.contains("> [!WARNING] literal"));
        assert!(converted.contains(":::tip\nReal\nbody\n\n:::"));
    }

    #[test]
    fn inbound_converts_supported_outline_callouts() {
        let source = ":::success\nShipped\n\n:::\n\n:::warning\nCareful\n:::";
        assert_eq!(
            outline_to_obsidian_markdown(source),
            "> [!success]\n> Shipped\n>\n\n> [!warning]\n> Careful"
        );
    }

    #[test]
    fn toc_removal_is_opt_in_and_limited_to_heading_link_lists() {
        let source = "- [[#Intro]]\n- [[#Setup]]\n\n- [[Other note]]\n\n# Intro\nBody";
        assert_eq!(
            obsidian_to_outline_markdown(source, OutlineMarkdownOptions::default()),
            source
        );
        assert_eq!(
            obsidian_to_outline_markdown(source, OutlineMarkdownOptions { remove_toc: true }),
            "- [[Other note]]\n\n# Intro\nBody"
        );
    }

    #[test]
    fn link_destination_translation_works_in_both_directions() {
        let outbound = rewrite_markdown_link_destinations(
            "See [Guide](Guides/Guide.md#setup) and [web](https://example.com)",
            |destination| {
                (destination == "Guides/Guide.md#setup")
                    .then(|| "/doc/remote-guide#setup".to_string())
            },
        );
        assert_eq!(
            outbound,
            "See [Guide](/doc/remote-guide#setup) and [web](https://example.com)"
        );

        let inbound = outline_document_links_to_obsidian(&outbound, |remote_id| {
            (remote_id == "remote-guide").then(|| "Guides/Guide".to_string())
        });
        assert_eq!(
            inbound,
            "See [[Guides/Guide#setup|Guide]] and [web](https://example.com)"
        );
    }

    #[test]
    fn malformed_frontmatter_is_preserved() {
        let source = "---\ntags: [unfinished\n# Body";
        assert_eq!(
            obsidian_to_outline_markdown(source, OutlineMarkdownOptions::default()),
            source
        );
    }
}
