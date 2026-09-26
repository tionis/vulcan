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
        SemanticBlockKind::List => lines
            .iter()
            .enumerate()
            .filter_map(|(index, &(start, end))| {
                let (block_id, offset) = trailing_block_id(source, start, end)?;
                Some(RawBlockRef {
                    block_id_text: block_id,
                    block_id_byte_offset: offset,
                    target_block_byte_start: start,
                    target_block_byte_end: list_item_end(source, &lines, index),
                })
            })
            .collect(),
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
    fn carets_that_are_not_trailing_ids_are_ignored() {
        assert!(parsed_refs("x^2 is math\n\nmid ^id text\n\n`code ^id`\n").is_empty());
    }
}
