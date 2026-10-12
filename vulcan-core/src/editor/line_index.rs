//! Byte offset ↔ line/column conversion for editor protocols.
//!
//! Editor protocols address text by zero-based line and a column counted in
//! a negotiated encoding (UTF-8 bytes, UTF-16 code units, or Unicode scalar
//! values). Vulcan's parser reports byte offsets, so every editor-facing
//! position must be converted through this index. CLI JSON columns are
//! 1-based scalar counts and are never a substitute.

/// The unit an editor column is counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    /// UTF-8 code units (bytes).
    Utf8,
    /// UTF-16 code units, the LSP default.
    Utf16,
    /// Unicode scalar values.
    Utf32,
}

/// A zero-based line and an encoding-dependent column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    pub line: u32,
    pub column: u32,
}

/// Line starts of one text, for converting between byte offsets and
/// editor positions.
///
/// Lines end at `\n`; a preceding `\r` stays part of the line's content, as
/// in LSP. Columns past the end of a line clamp to the line end, and lines
/// past the end of the text clamp to the text end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    len: usize,
    line_starts: Vec<usize>,
}

impl LineIndex {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self {
            len: text.len(),
            line_starts,
        }
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Convert a byte offset of `text` into a position.
    ///
    /// Offsets inside a multi-byte character round down to its start, and
    /// offsets past the end clamp to the end. `text` must be the text this
    /// index was built from.
    #[must_use]
    pub fn position(&self, text: &str, offset: usize, encoding: PositionEncoding) -> TextPosition {
        let offset = floor_char_boundary(text, offset.min(self.len));
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let line_start = self.line_starts[line];
        let column = encoded_len(&text[line_start..offset], encoding);
        TextPosition {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(column).unwrap_or(u32::MAX),
        }
    }

    /// Convert a position into a byte offset of `text`.
    ///
    /// Columns that fall inside a character (such as the second half of a
    /// UTF-16 surrogate pair) round down to that character's start.
    #[must_use]
    pub fn offset(&self, text: &str, position: TextPosition, encoding: PositionEncoding) -> usize {
        let line = position.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.len;
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len, |next| next - 1);
        let line_text = &text[line_start..line_end];
        let target = position.column as usize;
        let mut consumed = 0;
        for (byte_index, character) in line_text.char_indices() {
            if consumed >= target {
                return line_start + byte_index;
            }
            let width = char_len(character, encoding);
            if consumed + width > target {
                return line_start + byte_index;
            }
            consumed += width;
        }
        line_end
    }
}

fn encoded_len(text: &str, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => text.len(),
        PositionEncoding::Utf16 => text.encode_utf16().count(),
        PositionEncoding::Utf32 => text.chars().count(),
    }
}

fn char_len(character: char, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => character.len_utf8(),
        PositionEncoding::Utf16 => character.len_utf16(),
        PositionEncoding::Utf32 => 1,
    }
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "ab\n日本語 [[x]]\r\n😀z\n";

    fn pos(line: u32, column: u32) -> TextPosition {
        TextPosition { line, column }
    }

    #[test]
    fn converts_offsets_in_every_encoding() {
        let index = LineIndex::new(TEXT);
        assert_eq!(index.line_count(), 4);
        let z = TEXT.find('z').unwrap();
        assert_eq!(index.position(TEXT, z, PositionEncoding::Utf8), pos(2, 4));
        assert_eq!(index.position(TEXT, z, PositionEncoding::Utf16), pos(2, 2));
        assert_eq!(index.position(TEXT, z, PositionEncoding::Utf32), pos(2, 1));
        let link = TEXT.find("[[").unwrap();
        assert_eq!(
            index.position(TEXT, link, PositionEncoding::Utf8),
            pos(1, 10)
        );
        assert_eq!(
            index.position(TEXT, link, PositionEncoding::Utf16),
            pos(1, 4)
        );
    }

    #[test]
    fn round_trips_every_character_boundary() {
        let index = LineIndex::new(TEXT);
        for encoding in [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ] {
            for (offset, _) in TEXT.char_indices().chain([(TEXT.len(), ' ')]) {
                let position = index.position(TEXT, offset, encoding);
                assert_eq!(
                    index.offset(TEXT, position, encoding),
                    offset,
                    "{encoding:?}"
                );
            }
        }
    }

    #[test]
    fn clamps_out_of_range_and_mid_character_positions() {
        let index = LineIndex::new(TEXT);
        let emoji = TEXT.find('😀').unwrap();
        // Inside the surrogate pair rounds down to the emoji.
        assert_eq!(
            index.offset(TEXT, pos(2, 1), PositionEncoding::Utf16),
            emoji
        );
        // Inside a multi-byte character rounds down too.
        assert_eq!(
            index.position(TEXT, emoji + 1, PositionEncoding::Utf16),
            pos(2, 0)
        );
        // Past the line end stops before the newline (the `\r` stays in the line).
        let crlf = TEXT.find("\r\n").unwrap();
        assert_eq!(
            index.offset(TEXT, pos(1, 99), PositionEncoding::Utf16),
            crlf + 1
        );
        assert_eq!(
            index.offset(TEXT, pos(9, 0), PositionEncoding::Utf16),
            TEXT.len()
        );
        assert_eq!(index.position(TEXT, 999, PositionEncoding::Utf8), pos(3, 0));
    }

    #[test]
    fn handles_empty_text() {
        let index = LineIndex::new("");
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.position("", 0, PositionEncoding::Utf16), pos(0, 0));
        assert_eq!(index.offset("", pos(3, 3), PositionEncoding::Utf16), 0);
    }
}
