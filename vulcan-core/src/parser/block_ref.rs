use crate::parser::types::{RawBlockRef, SemanticBlock};

#[must_use]
pub fn detect_block_refs(blocks: &[SemanticBlock]) -> Vec<RawBlockRef> {
    let mut refs = Vec::new();
    let mut previous_target = None;

    for block in blocks {
        let Some(block_id) = parse_block_id(&block.text) else {
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
    use crate::parser::types::SemanticBlockKind;

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

        let refs = detect_block_refs(&blocks);

        assert_eq!(refs.len(), 100_000);
        assert!(refs
            .iter()
            .all(|block_ref| block_ref.target_block_byte_start == 0));
    }
}
