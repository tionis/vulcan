use crate::parser::types::{RawBlockRef, SemanticBlock, SemanticBlockKind};

/// Detects Obsidian block identifiers.
///
/// Two forms are recognised: a standalone `^id` paragraph labels the preceding block, and a
/// trailing ` ^id` at the end of a paragraph, quote, or list-item line labels that block or item.
#[must_use]
pub fn detect_block_refs(source: &str, blocks: &[SemanticBlock]) -> Vec<RawBlockRef> {
    let mut refs = Vec::new();
    let mut previous_target = None;

    for block in blocks {
        let Some(block_id) = parse_block_id(&block.text) else {
            refs.extend(trailing_block_refs(source, block));
            previous_target = Some(block);
            continue;
        };
        let Some(target_block) = previous_target else {
            continue;
        };

        refs.push(RawBlockRef {
            block_id_text: block_id,
            block_id_byte_offset: block.byte_offset_start,
            target_block_byte_start: target_block.byte_offset_start,
            target_block_byte_end: target_block.byte_offset_end,
        });
    }

    refs
}

fn trailing_block_refs(source: &str, block: &SemanticBlock) -> Vec<RawBlockRef> {
    let Some(text) = source.get(block.byte_offset_start..block.byte_offset_end) else {
        return Vec::new();
    };
    let lines = line_spans(text, block.byte_offset_start);

    match block.block_kind {
        SemanticBlockKind::Paragraph | SemanticBlockKind::BlockQuote => lines
            .iter()
            .rev()
            .find(|(start, end)| !source[*start..*end].trim().is_empty())
            .and_then(|&(start, end)| trailing_block_id(source, start, end))
            .map(|(block_id, offset)| RawBlockRef {
                block_id_text: block_id,
                block_id_byte_offset: offset,
                target_block_byte_start: block.byte_offset_start,
                target_block_byte_end: block.byte_offset_end,
            })
            .into_iter()
            .collect(),
        SemanticBlockKind::List => list_trailing_block_refs(source, &lines),
        SemanticBlockKind::CodeBlock | SemanticBlockKind::HtmlBlock | SemanticBlockKind::Table => {
            Vec::new()
        }
    }
}

/// Absolute `(start, end)` byte spans of each line in `text`, excluding line terminators.
fn line_spans(text: &str, base_offset: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        spans.push((base_offset + start, base_offset + start + content.len()));
        start += line.len();
    }
    spans
}

fn trailing_block_id(source: &str, start: usize, end: usize) -> Option<(String, usize)> {
    let line = source[start..end].trim_end();
    let caret = line.rfind('^')?;
    let preceded_by_space = line[..caret]
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace);
    if !preceded_by_space || line[..caret].trim().is_empty() {
        return None;
    }
    let block_id = parse_block_id(&line[caret..])?;
    Some((block_id, start + caret))
}

/// Trailing IDs inside a list label the list item that owns the line: either the item's own
/// marker line or a continuation line of that item. Lines inside fenced or indented code in the
/// list are literal and never carry a block ID.
fn list_trailing_block_refs(source: &str, lines: &[(usize, usize)]) -> Vec<RawBlockRef> {
    let mut refs = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    // Index of the innermost open list item and its content column.
    let mut items: Vec<(usize, usize)> = Vec::new();

    for (index, &(start, end)) in lines.iter().enumerate() {
        let line = &source[start..end];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some((marker, length)) = fence {
            if fence_marker(trimmed).is_some_and(|(candidate, candidate_length)| {
                candidate == marker
                    && candidate_length >= length
                    && trimmed.trim_start_matches(marker).trim().is_empty()
            }) {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = fence_marker(trimmed) {
            fence = Some(opened);
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }

        items.retain(|&(item_index, _)| {
            let item = &source[lines[item_index].0..lines[item_index].1];
            indent > item.len() - item.trim_start().len()
        });
        if let Some(width) = list_marker_width(trimmed) {
            items.push((index, indent + width));
        } else if items
            .last()
            .is_none_or(|&(_, content_column)| indent >= content_column + 4)
        {
            // Not inside an item's paragraph text: indented code or unrelated content.
            continue;
        }

        let Some((block_id, offset)) = trailing_block_id(source, start, end) else {
            continue;
        };
        let Some(&(item_index, _)) = items.last() else {
            continue;
        };
        refs.push(RawBlockRef {
            block_id_text: block_id,
            block_id_byte_offset: offset,
            target_block_byte_start: lines[item_index].0,
            target_block_byte_end: list_item_end(source, lines, item_index),
        });
    }

    refs
}

/// Width of a list marker plus its following space (`- `, `12. `), if the line starts one.
fn list_marker_width(trimmed: &str) -> Option<usize> {
    let bytes = trimmed.as_bytes();
    let marker_len = match bytes.first()? {
        b'-' | b'*' | b'+' => 1,
        b'0'..=b'9' => {
            let digits = bytes
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if digits > 9 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    matches!(bytes.get(marker_len), Some(b' ' | b'\t')).then_some(marker_len + 1)
}

/// A fence opener or closer: at least three backticks or tildes.
fn fence_marker(trimmed: &str) -> Option<(char, usize)> {
    let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = trimmed.chars().take_while(|&c| c == marker).count();
    (length >= 3).then_some((marker, length))
}

/// A list item spans its own line plus following lines indented deeper than it.
fn list_item_end(source: &str, lines: &[(usize, usize)], index: usize) -> usize {
    let indent = |&(start, end): &(usize, usize)| {
        let line = &source[start..end];
        line.len() - line.trim_start().len()
    };
    let item_indent = indent(&lines[index]);
    let mut item_end = lines[index].1;
    for line in &lines[index + 1..] {
        if source[line.0..line.1].trim().is_empty() {
            continue;
        }
        if indent(line) <= item_indent {
            break;
        }
        item_end = line.1;
    }
    item_end
}

#[must_use]
pub fn is_block_id_block(block: &SemanticBlock) -> bool {
    parse_block_id(&block.text).is_some()
}

#[must_use]
pub fn parse_block_id(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let block_id = trimmed.strip_prefix('^')?;
    if block_id.is_empty()
        || !block_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }

    Some(block_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: String, offset: usize) -> SemanticBlock {
        SemanticBlock {
            block_kind: SemanticBlockKind::Paragraph,
            text,
            byte_offset_start: offset,
            byte_offset_end: offset + 1,
            heading_path: Vec::new(),
            code_language: None,
        }
    }

    #[test]
    fn long_runs_of_block_ids_reuse_the_previous_content_block() {
        let mut blocks = vec![block("target".to_string(), 0)];
        blocks.extend((0..100_000).map(|index| block(format!("^id-{index}"), index + 1)));

        let refs = detect_block_refs(&"x".repeat(100_002), &blocks);

        assert_eq!(refs.len(), 100_000);
        assert!(refs
            .iter()
            .all(|block_ref| block_ref.target_block_byte_start == 0));
    }

    fn parsed_refs(source: &str) -> Vec<(String, String)> {
        crate::parser::parse_document(source, &crate::config::VaultConfig::default())
            .block_refs
            .into_iter()
            .map(|block_ref| {
                (
                    block_ref.block_id_text,
                    source[block_ref.target_block_byte_start..block_ref.target_block_byte_end]
                        .to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn trailing_ids_label_paragraphs_and_list_items() {
        let source = concat!(
            "First line\nlast line ^para-1\n\n",
            "- item one ^item-1\n  - child\n- item two\n\n",
            "> quoted ^quote\n",
        );

        assert_eq!(
            parsed_refs(source),
            vec![
                (
                    "para-1".to_string(),
                    "First line\nlast line ^para-1\n".to_string()
                ),
                (
                    "item-1".to_string(),
                    "- item one ^item-1\n  - child".to_string()
                ),
                ("quote".to_string(), "> quoted ^quote\n".to_string()),
            ]
        );
    }

    #[test]
    fn list_code_is_literal_and_continuations_label_their_item() {
        assert!(parsed_refs(concat!(
            "- item\n  ```\n  code ^fenced\n  ```\n",
            "- other\n\n        indented code ^indented\n",
        ))
        .is_empty());
        assert_eq!(
            parsed_refs("- item\n  more text ^cont\n  - child\n- next\n"),
            vec![(
                "cont".to_string(),
                "- item\n  more text ^cont\n  - child".to_string()
            )]
        );
        assert_eq!(
            parsed_refs("1. first\n2. second ^two\n"),
            vec![("two".to_string(), "2. second ^two".to_string())]
        );
    }

    #[test]
    fn carets_that_are_not_trailing_ids_are_ignored() {
        assert!(parsed_refs("x^2 is math\n\nmid ^id text\n\n`code ^id`\n").is_empty());
    }
}
