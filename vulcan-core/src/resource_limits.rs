pub(crate) const MAX_QUERY_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_EXPRESSION_OUTPUT_CHARS: usize = 256 * 1024;

pub(crate) fn ensure_query_input(source: &str) -> Result<(), String> {
    if source.len() > MAX_QUERY_INPUT_BYTES {
        return Err(format!(
            "query input exceeds maximum size of {MAX_QUERY_INPUT_BYTES} bytes"
        ));
    }
    Ok(())
}

pub(crate) fn checked_repeat(text: &str, count: usize) -> Option<String> {
    let output_chars = text.chars().count().checked_mul(count)?;
    (output_chars <= MAX_EXPRESSION_OUTPUT_CHARS).then(|| text.repeat(count))
}

pub(crate) fn ensure_expression_output_chars(length: usize) -> Result<(), String> {
    if length > MAX_EXPRESSION_OUTPUT_CHARS {
        return Err(format!(
            "expression output exceeds maximum length of {MAX_EXPRESSION_OUTPUT_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct WikilinkEndIndex {
    next_closing_bracket: Vec<usize>,
}

impl WikilinkEndIndex {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        let mut next_closing_bracket = vec![bytes.len(); bytes.len() + 1];
        let mut next = bytes.len();
        for index in (0..bytes.len()).rev() {
            if bytes[index] == b']' {
                next = index;
            }
            next_closing_bracket[index] = next;
        }
        Self {
            next_closing_bracket,
        }
    }

    pub(crate) fn end(&self, bytes: &[u8], start: usize) -> Option<usize> {
        let opening = start + usize::from(bytes.get(start) == Some(&b'!'));
        if bytes.get(opening..opening + 2) != Some(b"[[") {
            return None;
        }
        let closing = *self.next_closing_bracket.get(opening + 2)?;
        (bytes.get(closing..closing + 2) == Some(b"]]")).then_some(closing + 2)
    }
}
