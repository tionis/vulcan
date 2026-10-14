//! Templater source segmentation and JavaScript program compilation.
//!
//! Templater runs a whole template as one async function: text becomes
//! `tR += "…"`, `<% expr %>` appends the awaited expression, and `<%* code %>`
//! is spliced in verbatim. Compiling the same way lets declarations, `if`
//! blocks, and loops span tags, and makes `await` behave like JavaScript.

use super::{parse_templater_tag, TemplaterTag, TrimMode};

/// File name the compiled program is evaluated under, so stack frames that
/// belong to template code can be told apart from prelude and user scripts.
#[cfg(feature = "js_runtime")]
pub(super) const TEMPLATE_PROGRAM_FILENAME: &str = "vulcan-template.js";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TemplateSegment<'a> {
    /// Literal text with tag trim markers already applied.
    Text(&'a str),
    /// A tag and the 1-based template line its body starts on.
    Tag { tag: TemplaterTag<'a>, line: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnterminatedTag {
    pub(super) line: usize,
}

pub(super) fn split_templater_segments(
    source: &str,
) -> Result<Vec<TemplateSegment<'_>>, UnterminatedTag> {
    let mut segments = Vec::new();
    let mut cursor = 0_usize;
    let mut right_trim = TrimMode::None;
    while let Some(relative) = source[cursor..].find("<%") {
        let start = cursor + relative;
        let (tag, next) = parse_templater_tag(source, start).ok_or(UnterminatedTag {
            line: line_at(source, start),
        })?;
        let text_start = trim_text_start(source, cursor, start, right_trim);
        let text = trim_text_end(&source[text_start..start], tag.left_trim);
        if !text.is_empty() {
            segments.push(TemplateSegment::Text(text));
        }
        let line = line_at(source, body_offset(source, &tag).unwrap_or(start));
        right_trim = tag.right_trim;
        segments.push(TemplateSegment::Tag { tag, line });
        cursor = next;
    }
    let text_start = trim_text_start(source, cursor, source.len(), right_trim);
    if text_start < source.len() {
        segments.push(TemplateSegment::Text(&source[text_start..]));
    }
    Ok(segments)
}

fn trim_text_start(source: &str, start: usize, end: usize, trim: TrimMode) -> usize {
    let text = &source[start..end];
    let skipped = match trim {
        TrimMode::All => text.len() - text.trim_start().len(),
        TrimMode::Newline if text.starts_with("\r\n") => 2,
        TrimMode::Newline if text.starts_with('\n') => 1,
        TrimMode::None | TrimMode::Newline => 0,
    };
    start + skipped
}

fn trim_text_end(text: &str, trim: TrimMode) -> &str {
    match trim {
        TrimMode::None => text,
        TrimMode::All => text.trim_end(),
        TrimMode::Newline => text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text),
    }
}

fn body_offset(source: &str, tag: &TemplaterTag<'_>) -> Option<usize> {
    let base = source.as_ptr() as usize;
    let body = tag.body.as_ptr() as usize;
    (body >= base && body <= base + source.len()).then(|| body - base)
}

/// 1-based line number of a byte offset.
pub(super) fn line_at(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())].matches('\n').count() + 1
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProgramPiece<'a> {
    Text(String),
    Code { body: &'a str, line: usize },
    Expression { body: &'a str, line: usize },
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompiledProgram {
    pub(super) source: String,
    /// Template line for each compiled line (index 0 is compiled line 1);
    /// `0` marks scaffolding that has no template counterpart.
    line_map: Vec<usize>,
}

#[cfg(feature = "js_runtime")]
impl CompiledProgram {
    /// Template line for a compiled line. Scaffolding and text appends
    /// cannot throw, but `QuickJS` sometimes reports an error (such as an
    /// undefined global) on the statement before the faulting one, so an
    /// unmapped line resolves to the next mapped one.
    pub(super) fn template_line(&self, compiled_line: usize) -> Option<usize> {
        self.line_map
            .get(compiled_line.checked_sub(1)?..)?
            .iter()
            .copied()
            .find(|line| *line > 0)
    }
}

#[cfg(feature = "js_runtime")]
pub(super) fn compile_templater_program(pieces: &[ProgramPiece<'_>]) -> CompiledProgram {
    let mut program = ProgramBuilder::default();
    program.line("(async () => {", 0);
    program.line("let tR = \"\";", 0);
    for piece in pieces {
        match piece {
            ProgramPiece::Text(text) => {
                let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
                program.line(&format!("tR += {literal};"), 0);
            }
            ProgramPiece::Code { body, line } => program.block("", body, "", *line),
            ProgramPiece::Expression { body, line } => program.block(
                "tR += __vulcanTemplaterStringify(await (",
                body,
                "\n));",
                *line,
            ),
        }
    }
    program.line("return tR;", 0);
    program.line("})()", 0);
    CompiledProgram {
        source: program.source,
        line_map: program.line_map,
    }
}

#[cfg(feature = "js_runtime")]
#[derive(Default)]
struct ProgramBuilder {
    source: String,
    line_map: Vec<usize>,
}

#[cfg(feature = "js_runtime")]
impl ProgramBuilder {
    fn line(&mut self, text: &str, template_line: usize) {
        self.source.push_str(text);
        self.source.push('\n');
        self.line_map.push(template_line);
    }

    /// Splices `body` in line for line so compiled lines map back onto the
    /// template; `suffix` starts with a newline when the body may end in a
    /// `//` comment that would otherwise swallow it.
    fn block(&mut self, prefix: &str, body: &str, suffix: &str, first_line: usize) {
        let body_lines = body.matches('\n').count();
        let text = format!("{prefix}{body}{suffix}");
        for (offset, line) in text.split('\n').enumerate() {
            self.line(line, first_line + offset.min(body_lines));
        }
    }
}

#[cfg(feature = "js_runtime")]
/// Location of the innermost template frame in a `QuickJS` stack trace.
pub(super) fn program_line_from_stack(stack: &str) -> Option<usize> {
    stack.lines().find_map(|frame| {
        let rest =
            &frame[frame.find(TEMPLATE_PROGRAM_FILENAME)? + TEMPLATE_PROGRAM_FILENAME.len()..];
        let digits: String = rest
            .strip_prefix(':')?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

#[cfg(feature = "js_runtime")]
/// First stack frame outside the template program, such as a user script,
/// as `in helper (Scripts/helper.js)`. Line numbers are dropped because
/// `QuickJS` reports a function's start position for frames inside one.
pub(super) fn first_external_frame(stack: &str) -> Option<String> {
    stack.lines().map(str::trim).find_map(|frame| {
        let frame = frame.strip_prefix("at ")?;
        let (function, location) = match frame.split_once(" (") {
            Some((function, location)) => (Some(function), location.strip_suffix(')')?),
            None => (None, frame),
        };
        let (file, _) = location.split_once(':')?;
        if file == TEMPLATE_PROGRAM_FILENAME
            || file == "templater-prelude.js"
            || file == "templater-hooks.js"
            || file.starts_with('<')
        {
            return None;
        }
        Some(match function {
            Some(function) if function != "<anonymous>" => format!("in {function} ({file})"),
            _ => format!("in {file}"),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(segments: &[TemplateSegment<'_>]) -> Vec<String> {
        segments
            .iter()
            .map(|segment| match segment {
                TemplateSegment::Text(text) => format!("T:{text}"),
                TemplateSegment::Tag { tag, line } => format!("{line}:{}", tag.body),
            })
            .collect()
    }

    #[test]
    fn splits_text_and_tags_with_lines() {
        let segments = split_templater_segments("a\n<% one %>\nb\n<%* two\nthree %>c").unwrap();
        assert_eq!(
            texts(&segments),
            ["T:a\n", "2:one", "T:\nb\n", "4:two\nthree", "T:c"]
        );
    }

    #[test]
    fn applies_trim_markers_to_adjacent_text() {
        let segments = split_templater_segments("a\n<%- x -%>\nb  \n\n<%_ y _%>  \n c").unwrap();
        assert_eq!(texts(&segments), ["T:a", "2:x", "T:b", "5:y", "T:c"]);
    }

    #[test]
    fn reports_the_line_of_an_unterminated_tag() {
        assert_eq!(
            split_templater_segments("a\nb <% oops").unwrap_err(),
            UnterminatedTag { line: 2 }
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn compiled_lines_map_back_to_template_lines() {
        let program = compile_templater_program(&[
            ProgramPiece::Text("intro\n".to_string()),
            ProgramPiece::Code {
                body: "const a = 1;\nconst b = 2;",
                line: 3,
            },
            ProgramPiece::Expression {
                body: "a + b // sum",
                line: 7,
            },
        ]);
        let lines: Vec<&str> = program.source.lines().collect();
        let find = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap() + 1;
        assert_eq!(program.template_line(find("const a")), Some(3));
        assert_eq!(program.template_line(find("const b")), Some(4));
        assert_eq!(program.template_line(find("a + b")), Some(7));
        assert_eq!(program.template_line(1), Some(3));
        assert_eq!(program.template_line(lines.len()), None);
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn finds_template_and_external_frames() {
        let stack =
            "    at fail (Scripts/helper.js:3:9)\n    at <anonymous> (vulcan-template.js:12:5)\n";
        assert_eq!(program_line_from_stack(stack), Some(12));
        assert_eq!(
            first_external_frame(stack).as_deref(),
            Some("in fail (Scripts/helper.js)")
        );
    }
}
