use crate::parser::types::RawTag;

/// Extracts Obsidian inline tags (`#tag`, `#nested/tag`) from visible text.
///
/// Tag bodies accept Unicode letters and digits plus `/`, `_`, and `-`, and must contain at
/// least one non-numeric character: `#1984` is not a tag, while `#y1984` and `#tâche` are.
#[must_use]
pub fn extract_inline_tags(text: &str, base_offset: usize) -> Vec<RawTag> {
    let mut tags = Vec::new();
    let mut previous = None;
    let mut chars = text.char_indices().peekable();

    while let Some((index, character)) = chars.next() {
        if character == '#' && previous.is_none_or(|previous| !is_tag_char(previous)) {
            let start = index + 1;
            let mut end = start;
            let mut last = character;
            while let Some(&(next_index, next)) = chars.peek() {
                if !is_tag_char(next) {
                    break;
                }
                end = next_index + next.len_utf8();
                last = next;
                chars.next();
            }

            let tag_text = &text[start..end];
            if !tag_text.is_empty() && !tag_text.chars().all(|c| c.is_numeric() || c == '/') {
                tags.push(RawTag {
                    tag_text: tag_text.to_string(),
                    byte_offset: base_offset + index,
                });
            }
            previous = Some(last);
            continue;
        }

        previous = Some(character);
    }

    tags
}

fn is_tag_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '/' | '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(text: &str) -> Vec<String> {
        extract_inline_tags(text, 0)
            .into_iter()
            .map(|tag| tag.tag_text)
            .collect()
    }

    #[test]
    fn nested_and_unicode_tags_are_extracted_whole() {
        assert_eq!(
            tags("#tâche #日本語 #project/sub-task_2 end"),
            vec!["tâche", "日本語", "project/sub-task_2"]
        );
    }

    #[test]
    fn purely_numeric_tags_are_rejected() {
        assert_eq!(tags("#123 #2024/05 #y1984 #1984x"), vec!["y1984", "1984x"]);
    }

    #[test]
    fn hashes_inside_words_are_not_tags() {
        assert_eq!(tags("issue#12 café#tag a#b # alone"), Vec::<String>::new());
    }

    #[test]
    fn byte_offsets_account_for_multibyte_text() {
        let found = extract_inline_tags("é #tag", 10);

        assert_eq!(found[0].byte_offset, 10 + "é ".len());
    }
}
