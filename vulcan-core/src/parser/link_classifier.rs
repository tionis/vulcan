use crate::parser::types::{LinkKind, OriginContext, RawLink};
use pulldown_cmark::LinkType;

const ATTACHMENT_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "tif", "tiff", "pdf", "mp3", "wav", "ogg",
    "mp4", "mov", "webm", "avi",
];

#[must_use]
pub fn classify_link(
    link_type: LinkType,
    is_image: bool,
    dest_url: &str,
    raw_text: String,
    display_text: Option<String>,
    byte_offset: usize,
) -> RawLink {
    if is_external(dest_url) {
        return RawLink {
            raw_text,
            link_kind: LinkKind::External,
            display_text,
            target_path_candidate: None,
            target_heading: None,
            target_block: None,
            origin_context: OriginContext::Body,
            byte_offset,
            is_note_embed: false,
        };
    }

    let (target_path_candidate, target_heading, target_block) = split_subpath(dest_url);
    let link_kind = if is_image {
        LinkKind::Embed
    } else if matches!(link_type, LinkType::WikiLink { .. }) {
        LinkKind::Wikilink
    } else {
        LinkKind::Markdown
    };
    let is_note_embed = is_image && looks_like_note_embed(target_path_candidate.as_deref());

    RawLink {
        raw_text,
        link_kind,
        display_text,
        target_path_candidate,
        target_heading,
        target_block,
        origin_context: OriginContext::Body,
        byte_offset,
        is_note_embed,
    }
}

#[must_use]
pub fn explicit_display_text(
    link_type: LinkType,
    _is_image: bool,
    display_text: &str,
) -> Option<String> {
    let trimmed = display_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    match link_type {
        LinkType::WikiLink { has_pothole } if !has_pothole => None,
        _ => Some(trimmed.to_string()),
    }
}

#[must_use]
pub fn split_subpath(dest_url: &str) -> (Option<String>, Option<String>, Option<String>) {
    let Some((path, fragment)) = dest_url.split_once('#') else {
        return (empty_to_none(dest_url), None, None);
    };

    if let Some(block) = fragment.strip_prefix('^') {
        (empty_to_none(path), None, Some(block.to_string()))
    } else {
        (empty_to_none(path), Some(fragment.to_string()), None)
    }
}

fn empty_to_none(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn looks_like_note_embed(target_path: Option<&str>) -> bool {
    let Some(target_path) = target_path else {
        return true;
    };
    let Some((_, extension)) = target_path.rsplit_once('.') else {
        return true;
    };

    !ATTACHMENT_EXTENSIONS
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

fn is_external(dest_url: &str) -> bool {
    dest_url.contains("://") || dest_url.starts_with("mailto:")
}
