use crate::parser::types::{RawBlockRef, SemanticBlock};

#[must_use]
pub fn detect_block_refs(blocks: &[SemanticBlock]) -> Vec<RawBlockRef> {
    let mut refs = Vec::new();

    for (index, block) in blocks.iter().enumerate() {
        let Some(block_id) = parse_block_id(&block.text) else {
            continue;
        };
        let Some(target_block) = blocks[..index]
            .iter()
            .rev()
            .find(|candidate| !is_block_id_block(candidate))
        else {
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
