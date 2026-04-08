use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use std::fmt::Write as _;

pub(crate) fn render_terminal_markdown(markdown: &str, use_color: bool) -> String {
    render_terminal_markdown_lines(markdown, use_color).join("\n")
}

pub(crate) fn render_terminal_markdown_lines(markdown: &str, use_color: bool) -> Vec<String> {
    let mut renderer = Renderer::new(use_color);
    let parser = Parser::new_ext(markdown, Options::all());
    for event in parser {
        renderer.push(event);
    }
    renderer.finish()
}

#[allow(clippy::struct_excessive_bools)]
struct Renderer {
    lines: Vec<String>,
    current: String,
    code_block: String,
    metadata_block: String,
    use_color: bool,
    list_stack: Vec<ListState>,
    active_heading: Option<u8>,
    in_item: bool,
    in_code_block: bool,
    in_metadata_block: bool,
    blockquote_depth: usize,
    table: Option<TableState>,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next_index: Option<u64>,
}

#[derive(Debug, Default)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    in_header: bool,
}

impl Renderer {
    fn new(use_color: bool) -> Self {
        Self {
            lines: Vec::new(),
            current: String::new(),
            code_block: String::new(),
            metadata_block: String::new(),
            use_color,
            list_stack: Vec::new(),
            active_heading: None,
            in_item: false,
            in_code_block: false,
            in_metadata_block: false,
            blockquote_depth: 0,
            table: None,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn push(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                self.flush_current();
                self.active_heading = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                let heading = self.current.trim();
                if !heading.is_empty() {
                    self.push_line(style_heading(
                        heading,
                        self.active_heading.unwrap_or(1),
                        self.use_color,
                    ));
                    self.push_blank_line();
                }
                self.current.clear();
                self.active_heading = None;
            }
            Event::Start(Tag::MetadataBlock(_)) => {
                self.flush_current();
                self.in_metadata_block = true;
            }
            Event::End(TagEnd::MetadataBlock(_)) => {
                let body = self.metadata_block.trim_end().to_string();
                if !body.is_empty() {
                    self.push_line(style_metadata_line("---", self.use_color));
                    for line in body.lines() {
                        self.push_line(style_metadata_line(line, self.use_color));
                    }
                    self.push_line(style_metadata_line("---", self.use_color));
                    self.push_blank_line();
                }
                self.metadata_block.clear();
                self.in_metadata_block = false;
            }
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => self.flush_current(),
            Event::Start(Tag::List(start)) => {
                self.flush_current();
                self.list_stack.push(ListState { next_index: start });
            }
            Event::End(TagEnd::List(_)) => {
                self.flush_current();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.push_blank_line();
                }
            }
            Event::Start(Tag::Item) => {
                self.flush_current();
                self.in_item = true;
            }
            Event::End(TagEnd::Item) => {
                let text = self.current.trim().to_string();
                if !text.is_empty() {
                    let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
                    let marker = self.list_marker();
                    self.push_line(format!("{indent}{marker} {text}"));
                }
                self.current.clear();
                self.in_item = false;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush_current();
                self.in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                let body = self.code_block.trim_end().to_string();
                if !body.is_empty() {
                    for line in body.lines() {
                        self.push_line(style_code_block(line, self.use_color));
                    }
                    self.push_blank_line();
                }
                self.code_block.clear();
                self.in_code_block = false;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.flush_current();
                self.blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush_current();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.push_blank_line();
            }
            Event::Start(Tag::Table(alignments)) => {
                self.flush_current();
                self.table = Some(TableState {
                    alignments,
                    ..TableState::default()
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = self.table.take() {
                    for line in render_table_lines(&table, self.use_color) {
                        self.push_line(line);
                    }
                    self.push_blank_line();
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = self.table.as_mut() {
                    table.in_header = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = self.table.as_mut() {
                    if table.header.is_empty() && !table.current_row.is_empty() {
                        table.header = std::mem::take(&mut table.current_row);
                    }
                    table.in_header = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = self.table.as_mut() {
                    if table.in_header {
                        table.header = std::mem::take(&mut table.current_row);
                    } else if !table.current_row.is_empty() {
                        table.rows.push(std::mem::take(&mut table.current_row));
                    }
                }
            }
            Event::Start(Tag::TableCell) => {
                self.current.clear();
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.push(self.current.trim().to_string());
                    self.current.clear();
                }
            }
            Event::Code(code) => {
                self.current
                    .push_str(&style_inline_code(&code, self.use_color));
            }
            Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                if self.in_metadata_block {
                    self.metadata_block.push_str(&text);
                } else if self.in_code_block {
                    self.code_block.push_str(&text);
                } else {
                    self.current.push_str(&text);
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                if self.in_metadata_block {
                    self.metadata_block.push_str(&text);
                } else {
                    self.current.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.in_metadata_block {
                    self.metadata_block.push('\n');
                } else if self.in_code_block {
                    self.code_block.push('\n');
                } else if self.table.is_some() || self.in_item {
                    self.current.push_str("  ");
                } else {
                    self.current.push('\n');
                }
            }
            Event::Rule => {
                self.flush_current();
                self.push_line(style_rule(self.use_color));
                self.push_blank_line();
            }
            Event::TaskListMarker(done) => {
                let marker = if done { "[x] " } else { "[ ] " };
                self.current.push_str(marker);
            }
            Event::FootnoteReference(reference) => {
                let _ = write!(self.current, "[^{reference}]");
            }
            _ => {}
        }
    }

    fn flush_current(&mut self) {
        let text = self.current.trim().to_string();
        if text.is_empty() {
            self.current.clear();
            return;
        }
        if self.in_item || self.table.is_some() {
            return;
        }
        self.push_line(apply_blockquote_prefix(&text, self.blockquote_depth));
        self.push_blank_line();
        self.current.clear();
    }

    fn list_marker(&mut self) -> String {
        let Some(state) = self.list_stack.last_mut() else {
            return "-".to_string();
        };

        if let Some(index) = state.next_index.as_mut() {
            let marker = format!("{index}.");
            *index += 1;
            marker
        } else {
            "-".to_string()
        }
    }

    fn push_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn push_blank_line(&mut self) {
        if self.lines.last().is_some_and(String::is_empty) {
            return;
        }
        self.lines.push(String::new());
    }

    fn finish(mut self) -> Vec<String> {
        self.flush_current();
        while self.lines.last().is_some_and(String::is_empty) {
            self.lines.pop();
        }
        self.lines
    }
}

fn apply_blockquote_prefix(text: &str, depth: usize) -> String {
    if depth == 0 {
        return text.to_string();
    }

    let prefix = format!("{} ", ">".repeat(depth));
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_table_lines(table: &TableState, use_color: bool) -> Vec<String> {
    let column_count = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if column_count == 0 {
        return Vec::new();
    }

    let widths = (0..column_count)
        .map(|index| {
            let header_width = table
                .header
                .get(index)
                .map_or(0, |cell| cell.chars().count());
            table
                .rows
                .iter()
                .filter_map(|row| row.get(index))
                .map(|cell| cell.chars().count())
                .fold(header_width, usize::max)
        })
        .collect::<Vec<_>>();

    let mut lines = Vec::with_capacity(table.rows.len() + 2);
    lines.push(render_table_row(
        &table.header,
        &widths,
        &table.alignments,
        use_color,
        true,
    ));
    lines.push(render_table_separator(
        &widths,
        &table.alignments,
        use_color,
    ));
    lines.extend(
        table
            .rows
            .iter()
            .map(|row| render_table_row(row, &widths, &table.alignments, use_color, false)),
    );
    lines
}

fn render_table_row(
    row: &[String],
    widths: &[usize],
    alignments: &[Alignment],
    use_color: bool,
    is_header: bool,
) -> String {
    let cells = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let value = row.get(index).cloned().unwrap_or_default();
            let aligned = align_cell(&value, *width, alignments.get(index).copied());
            if is_header {
                style_table_header(&aligned, use_color)
            } else {
                aligned
            }
        })
        .collect::<Vec<_>>();
    format!("| {} |", cells.join(" | "))
}

fn render_table_separator(widths: &[usize], alignments: &[Alignment], use_color: bool) -> String {
    let cells = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let width = (*width).max(3);
            let cell = match alignments.get(index).copied().unwrap_or(Alignment::None) {
                Alignment::Left => format!(":{:-<width$}", "", width = width - 1),
                Alignment::Center => {
                    if width == 3 {
                        ":=:".to_string()
                    } else {
                        format!(":{:-<inner$}:", "", inner = width - 2)
                    }
                }
                Alignment::Right => format!("{:-<width$}:", "", width = width - 1),
                Alignment::None => format!("{:-<width$}", "", width = width),
            };
            if use_color {
                format!("\u{1b}[2m{cell}\u{1b}[0m")
            } else {
                cell
            }
        })
        .collect::<Vec<_>>();
    format!("| {} |", cells.join(" | "))
}

fn align_cell(value: &str, width: usize, alignment: Option<Alignment>) -> String {
    let cell_width = value.chars().count();
    if cell_width >= width {
        return value.to_string();
    }

    let padding = width - cell_width;
    match alignment.unwrap_or(Alignment::Left) {
        Alignment::Right => format!("{}{}", " ".repeat(padding), value),
        Alignment::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{}{}", " ".repeat(left), value, " ".repeat(right))
        }
        Alignment::Left | Alignment::None => format!("{value}{}", " ".repeat(padding)),
    }
}

fn style_heading(text: &str, _level: u8, use_color: bool) -> String {
    if use_color {
        format!("\u{1b}[1;36m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn style_table_header(text: &str, use_color: bool) -> String {
    if use_color {
        format!("\u{1b}[1m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn style_inline_code(text: &str, use_color: bool) -> String {
    if use_color {
        format!("\u{1b}[1;36m{text}\u{1b}[0m")
    } else {
        format!("`{text}`")
    }
}

fn style_code_block(text: &str, use_color: bool) -> String {
    if use_color {
        format!("  \u{1b}[2m{text}\u{1b}[0m")
    } else {
        format!("  {text}")
    }
}

fn style_metadata_line(text: &str, use_color: bool) -> String {
    if use_color {
        format!("\u{1b}[2m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn style_rule(use_color: bool) -> String {
    if use_color {
        "\u{1b}[2m----------------------------------------\u{1b}[0m".to_string()
    } else {
        "----------------------------------------".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{render_terminal_markdown, render_terminal_markdown_lines};

    #[test]
    fn renders_markdown_without_raw_fences_or_heading_markers() {
        let rendered = render_terminal_markdown(
            "# Title\n\nParagraph with `code`.\n\n- one\n- two\n\n```js\nconst value = 1;\n```",
            false,
        );

        assert!(rendered.contains("Title"));
        assert!(rendered.contains("Paragraph with `code`."));
        assert!(rendered.contains("- one"));
        assert!(rendered.contains("const value = 1;"));
        assert!(!rendered.contains("# Title"));
        assert!(!rendered.contains("```"));
    }

    #[test]
    fn renders_markdown_tables_as_aligned_terminal_tables() {
        let rendered = render_terminal_markdown(
            "| Name | Hours |\n| --- | ---: |\n| Alpha | 2 |\n| Beta | 12 |",
            false,
        );

        assert!(rendered.contains("| Name  | Hours |"));
        assert!(rendered.contains("| ----- | ----: |"));
        assert!(rendered.contains("| Alpha |     2 |"));
        assert!(rendered.contains("| Beta  |    12 |"));
    }

    #[test]
    fn exposes_line_oriented_rendering_for_tui_reuse() {
        let lines = render_terminal_markdown_lines("> quoted\n\n1. first\n2. second", false);

        assert_eq!(lines, vec!["> quoted", "", "1. first", "2. second"]);
    }

    #[test]
    fn renders_yaml_frontmatter_before_body_content() {
        let rendered = render_terminal_markdown(
            "---\ntitle: Alpha\ntags:\n  - project\n---\n\n# Heading\n\nBody\n",
            false,
        );

        assert!(rendered.starts_with("---\ntitle: Alpha\ntags:\n  - project\n---\n\nHeading"));
        assert!(rendered.contains("Body"));
    }
}
