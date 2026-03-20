use crate::parser::types::RawTag;

#[must_use]
pub fn extract_inline_tags(text: &str, base_offset: usize) -> Vec<RawTag> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'#' && is_boundary(bytes, index) {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && is_tag_char(bytes[end]) {
                end += 1;
            }

            if end > start {
                tags.push(RawTag {
                    tag_text: text[start..end].to_string(),
                    byte_offset: base_offset + index,
                });
                index = end;
                continue;
            }
        }

        index += 1;
    }

    tags
}

fn is_boundary(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }

    !is_tag_char(bytes[index - 1])
}

fn is_tag_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-')
}
