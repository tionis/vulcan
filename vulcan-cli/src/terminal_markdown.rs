use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::fmt::Write as _;

pub(crate) fn render_terminal_markdown(markdown: &str, use_color: bool) -> String {
    let mut renderer = Renderer::new(use_color);
    let parser = Parser::new_ext(markdown, Options::all());
    for event in parser {
        renderer.push(event);
    }
    renderer.finish()
}

struct Renderer {
    output: String,
    current: String,
    code_block: String,
    use_color: bool,
    list_depth: usize,
    active_heading: Option<u8>,
    in_item: bool,
    in_code_block: bool,
}

impl Renderer {
    fn new(use_color: bool) -> Self {
        Self {
            output: String::new(),
            current: String::new(),
            code_block: String::new(),
            use_color,
            list_depth: 0,
            active_heading: None,
            in_item: false,
            in_code_block: false,
        }
    }

    fn push(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                self.flush_current();
                self.active_heading = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                let heading = self.current.trim();
                if !heading.is_empty() {
                    self.push_line(&style_heading(
                        heading,
                        self.active_heading.unwrap_or(1),
                        self.use_color,
                    ));
                    self.output.push('\n');
                }
                self.current.clear();
                self.active_heading = None;
            }
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => self.flush_current(),
            Event::Start(Tag::List(_)) => {
                self.flush_current();
                self.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                self.flush_current();
                self.list_depth = self.list_depth.saturating_sub(1);
                if self.list_depth == 0 {
                    self.output.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                self.flush_current();
                self.in_item = true;
            }
            Event::End(TagEnd::Item) => {
                let text = self.current.trim();
                if !text.is_empty() {
                    let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                    self.push_line(&format!("{indent}- {text}"));
                }
                self.current.clear();
                self.in_item = false;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                self.flush_current();
                self.in_code_block = true;
                if let CodeBlockKind::Fenced(language) = kind {
                    let _ = writeln!(self.code_block, "{language}");
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                let body = self.code_block.trim_end().to_string();
                if !body.is_empty() {
                    for line in body.lines() {
                        self.push_line(&style_code_block(line, self.use_color));
                    }
                    self.output.push('\n');
                }
                self.code_block.clear();
                self.in_code_block = false;
            }
            Event::Code(code) => {
                self.current
                    .push_str(&style_inline_code(&code, self.use_color));
            }
            Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                if self.in_code_block {
                    self.code_block.push_str(&text);
                } else {
                    self.current.push_str(&text);
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.current.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.in_code_block {
                    self.code_block.push('\n');
                } else {
                    self.current.push('\n');
                }
            }
            Event::Rule => {
                self.flush_current();
                self.push_line(&style_rule(self.use_color));
                self.output.push('\n');
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
        if self.in_item {
            return;
        }
        self.push_line(&text);
        self.output.push('\n');
        self.current.clear();
    }

    fn push_line(&mut self, line: &str) {
        self.output.push_str(line);
        self.output.push('\n');
    }

    fn finish(mut self) -> String {
        self.flush_current();
        self.output.trim_end().to_string()
    }
}

fn style_heading(text: &str, level: u8, use_color: bool) -> String {
    let _ = level;
    let prefix = "";
    if use_color {
        format!("{prefix}\u{1b}[1;36m{text}\u{1b}[0m")
    } else {
        format!("{prefix}{text}")
    }
}

fn style_inline_code(text: &str, use_color: bool) -> String {
    if use_color {
        format!("\u{1b}[2m`{text}`\u{1b}[0m")
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

fn style_rule(use_color: bool) -> String {
    if use_color {
        "\u{1b}[2m----------------------------------------\u{1b}[0m".to_string()
    } else {
        "----------------------------------------".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::render_terminal_markdown;

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
}
