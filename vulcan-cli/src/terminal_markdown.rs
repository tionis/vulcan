//! Markdown rendering for terminals and TUI panes.
//!
//! The renderer lays Markdown out as styled lines: block containers (quotes,
//! callouts, list items) contribute prefixes, paragraphs wrap to the
//! available width with hanging indents, and inline markup becomes span
//! styles. The same lines feed ANSI output for the CLI and ratatui lines for
//! TUI previews, so both surfaces show notes identically.

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd,
};
use std::io::IsTerminal as _;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

/// Semantic colors; each sink maps them to its own palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Heading,
    Code,
    Link,
    Muted,
    Info,
    Success,
    Warning,
    Danger,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SpanStyle {
    pub tone: Option<Tone>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub strike: bool,
}

impl SpanStyle {
    fn tone(tone: Tone) -> Self {
        Self {
            tone: Some(tone),
            ..Self::default()
        }
    }

    fn muted() -> Self {
        Self {
            tone: Some(Tone::Muted),
            dim: true,
            ..Self::default()
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            tone: other.tone.or(self.tone),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            dim: self.dim || other.dim,
            strike: self.strike || other.strike,
        }
    }

    fn is_plain(self) -> bool {
        self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

impl StyledSpan {
    fn new(text: impl Into<String>, style: SpanStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

pub(crate) type StyledLine = Vec<StyledSpan>;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TerminalMarkdownOptions {
    /// Wrap paragraphs to this many columns; `None` never wraps.
    pub width: Option<usize>,
    /// Use color and text attributes. Without them, structure is kept with
    /// plain-text conventions such as backticks around inline code.
    pub styled: bool,
}

/// The terminal width when stdout is an interactive terminal.
pub(crate) fn stdout_width() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    crossterm::terminal::size()
        .ok()
        .map(|(columns, _)| usize::from(columns))
        .filter(|columns| *columns >= 20)
}

pub(crate) fn render_terminal_markdown(markdown: &str, use_color: bool) -> String {
    render_terminal_markdown_with(
        markdown,
        TerminalMarkdownOptions {
            width: None,
            styled: use_color,
        },
    )
}

/// Render for stdout, wrapping to the terminal width when it is a TTY.
pub(crate) fn render_terminal_markdown_for_stdout(markdown: &str, use_color: bool) -> String {
    render_terminal_markdown_with(
        markdown,
        TerminalMarkdownOptions {
            width: stdout_width(),
            styled: use_color,
        },
    )
}

pub(crate) fn render_terminal_markdown_with(
    markdown: &str,
    options: TerminalMarkdownOptions,
) -> String {
    render_styled_lines(markdown, options)
        .iter()
        .map(|line| line_to_ansi(line, options.styled))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lay out `markdown` as styled lines.
pub(crate) fn render_styled_lines(
    markdown: &str,
    options: TerminalMarkdownOptions,
) -> Vec<StyledLine> {
    let mut renderer = Renderer::new(options);
    for event in Parser::new_ext(markdown, parser_options()) {
        renderer.push(event);
    }
    renderer.finish()
}

fn parser_options() -> Options {
    // Smart punctuation is deliberately off: a viewer must show the note's
    // own characters.
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_WIKILINKS
}

fn ansi_color(tone: Tone) -> &'static str {
    match tone {
        Tone::Heading => "36",
        Tone::Code => "33",
        Tone::Link => "34",
        Tone::Muted => "90",
        Tone::Info => "94",
        Tone::Success => "32",
        Tone::Warning => "93",
        Tone::Danger => "91",
    }
}

fn line_to_ansi(line: &StyledLine, styled: bool) -> String {
    let mut rendered = String::new();
    for span in line {
        if !styled || span.style.is_plain() {
            rendered.push_str(&span.text);
            continue;
        }
        let mut codes = Vec::new();
        for (enabled, code) in [
            (span.style.bold, "1"),
            (span.style.dim, "2"),
            (span.style.italic, "3"),
            (span.style.underline, "4"),
            (span.style.strike, "9"),
        ] {
            if enabled {
                codes.push(code);
            }
        }
        if let Some(tone) = span.style.tone {
            codes.push(ansi_color(tone));
        }
        rendered.push_str("\u{1b}[");
        rendered.push_str(&codes.join(";"));
        rendered.push('m');
        rendered.push_str(&span.text);
        rendered.push_str("\u{1b}[0m");
    }
    rendered.trim_end().to_string()
}

fn text_width(text: &str) -> usize {
    text.width()
}

fn line_width(line: &[StyledSpan]) -> usize {
    line.iter().map(|span| text_width(&span.text)).sum()
}

/// A block container that prefixes the lines inside it.
#[derive(Debug, Clone)]
struct Container {
    /// Prefix for the first line emitted inside the container.
    first: Vec<StyledSpan>,
    /// Prefix for every later line.
    rest: Vec<StyledSpan>,
    /// Prefix for blank separator lines.
    blank: Vec<StyledSpan>,
    used: bool,
    kind: ContainerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Quote,
    Item,
    Footnote,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next_index: Option<u64>,
}

#[derive(Debug, Default)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<Vec<StyledSpan>>,
    rows: Vec<Vec<Vec<StyledSpan>>>,
    current_row: Vec<Vec<StyledSpan>>,
    in_header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalloutState {
    /// A blockquote just opened; its first text may carry `[!type]`.
    Detect,
    /// Inline content belongs to the callout title until the line ends.
    Title,
}

struct Renderer {
    options: TerminalMarkdownOptions,
    lines: Vec<StyledLine>,
    inline: Vec<StyledSpan>,
    style_stack: Vec<SpanStyle>,
    containers: Vec<Container>,
    lists: Vec<ListState>,
    pending_item_marker: Option<(String, SpanStyle)>,
    pending_blank: bool,
    heading: Option<u8>,
    code_block: Option<(String, String)>,
    metadata: Option<String>,
    table: Option<TableState>,
    link_stack: Vec<Option<String>>,
    callout: Option<CalloutState>,
    callout_title: Vec<StyledSpan>,
    callout_kind: String,
    callout_probe: String,
}

impl Renderer {
    fn new(options: TerminalMarkdownOptions) -> Self {
        Self {
            options,
            lines: Vec::new(),
            inline: Vec::new(),
            style_stack: Vec::new(),
            containers: Vec::new(),
            lists: Vec::new(),
            pending_item_marker: None,
            pending_blank: false,
            heading: None,
            code_block: None,
            metadata: None,
            table: None,
            link_stack: Vec::new(),
            callout: None,
            callout_title: Vec::new(),
            callout_kind: String::new(),
            callout_probe: String::new(),
        }
    }

    fn styled(&self) -> bool {
        self.options.styled
    }

    fn current_style(&self) -> SpanStyle {
        self.style_stack
            .iter()
            .fold(SpanStyle::default(), |style, next| style.merge(*next))
    }

    #[allow(clippy::too_many_lines)]
    fn push(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some(metadata) = self.metadata.as_mut() {
                    metadata.push_str(&text);
                } else if let Some((_, body)) = self.code_block.as_mut() {
                    body.push_str(&text);
                } else {
                    self.push_text(&text);
                }
            }
            Event::Code(code) => {
                let text = if self.styled() {
                    code.to_string()
                } else {
                    format!("`{code}`")
                };
                let style = self.current_style().merge(SpanStyle::tone(Tone::Code));
                self.push_span(StyledSpan::new(text, style));
            }
            Event::InlineMath(math) => {
                let style = self.current_style().merge(SpanStyle::tone(Tone::Code));
                self.push_span(StyledSpan::new(format!("${math}$"), style));
            }
            Event::DisplayMath(math) => {
                self.flush_paragraph();
                self.emit_block_lines(
                    math.lines()
                        .map(|line| vec![StyledSpan::new(line, SpanStyle::tone(Tone::Code))])
                        .collect(),
                );
                self.pending_blank = true;
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if let Some((_, body)) = self.code_block.as_mut() {
                    body.push_str(&html);
                } else {
                    let text = html.trim_end_matches('\n');
                    if !text.is_empty() {
                        self.push_span(StyledSpan::new(text, SpanStyle::muted()));
                    }
                }
            }
            Event::SoftBreak => {
                if self.callout == Some(CalloutState::Title) {
                    self.finish_callout_title();
                } else if let Some((_, body)) = self.code_block.as_mut() {
                    body.push('\n');
                } else {
                    self.push_text(" ");
                }
            }
            Event::HardBreak => {
                if self.callout == Some(CalloutState::Title) {
                    self.finish_callout_title();
                } else {
                    self.push_span(StyledSpan::new("\n", SpanStyle::default()));
                }
            }
            Event::Rule => {
                self.flush_paragraph();
                let width = self.available_width().unwrap_or(40).min(80);
                self.emit_block_lines(vec![vec![StyledSpan::new(
                    "─".repeat(width),
                    SpanStyle::muted(),
                )]]);
                self.pending_blank = true;
            }
            Event::TaskListMarker(done) => {
                // The list marker already moved into the item prefix; replace
                // it with the checkbox so wrapped lines align after it.
                if let Some(container) = self.containers.last_mut() {
                    if container.kind == ContainerKind::Item && !container.used {
                        let (marker, style) = if done {
                            ("[x] ", SpanStyle::tone(Tone::Success))
                        } else {
                            ("[ ] ", SpanStyle::tone(Tone::Muted))
                        };
                        let indent = text_width(&spans_text(&container.rest));
                        container.first = vec![
                            StyledSpan::new(
                                " ".repeat(indent.saturating_sub(2)),
                                SpanStyle::default(),
                            ),
                            StyledSpan::new(marker, style),
                        ];
                        container.rest = vec![StyledSpan::new(
                            " ".repeat(indent.saturating_sub(2) + marker.len()),
                            SpanStyle::default(),
                        )];
                        if done {
                            self.style_stack.push(SpanStyle {
                                dim: true,
                                strike: self.options.styled,
                                ..SpanStyle::default()
                            });
                            self.pending_item_marker = Some((String::new(), SpanStyle::default()));
                        }
                    }
                }
            }
            Event::FootnoteReference(label) => {
                let style = self.current_style().merge(SpanStyle::tone(Tone::Link));
                self.push_span(StyledSpan::new(format!("[^{label}]"), style));
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.flush_paragraph(),
            Tag::Heading { level, .. } => {
                self.flush_paragraph();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(kind) => {
                self.flush_paragraph();
                let bar = vec![StyledSpan::new("│ ", SpanStyle::muted())];
                self.containers.push(Container {
                    first: bar.clone(),
                    rest: bar,
                    blank: vec![StyledSpan::new("│", SpanStyle::muted())],
                    used: false,
                    kind: ContainerKind::Quote,
                });
                if let Some(kind) = kind {
                    // GFM alerts have no title; the label stands alone.
                    self.begin_callout(gfm_alert_name(kind));
                    self.finish_callout_title();
                } else {
                    self.callout = Some(CalloutState::Detect);
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language.trim().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush_paragraph();
                self.lists.push(ListState { next_index: start });
            }
            Tag::Item => {
                self.flush_paragraph();
                let marker = self.next_list_marker();
                let width = text_width(&marker) + 1;
                let style = if self
                    .lists
                    .last()
                    .is_some_and(|list| list.next_index.is_some())
                {
                    SpanStyle::tone(Tone::Muted)
                } else {
                    SpanStyle::tone(Tone::Heading)
                };
                self.containers.push(Container {
                    first: vec![StyledSpan::new(format!("{marker} "), style)],
                    rest: vec![StyledSpan::new(" ".repeat(width), SpanStyle::default())],
                    blank: Vec::new(),
                    used: false,
                    kind: ContainerKind::Item,
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_paragraph();
                let marker = format!("[^{label}]: ");
                let width = text_width(&marker);
                self.containers.push(Container {
                    first: vec![StyledSpan::new(marker, SpanStyle::tone(Tone::Link))],
                    rest: vec![StyledSpan::new(" ".repeat(width), SpanStyle::default())],
                    blank: Vec::new(),
                    used: false,
                    kind: ContainerKind::Footnote,
                });
            }
            Tag::Table(alignments) => {
                self.flush_paragraph();
                self.table = Some(TableState {
                    alignments,
                    ..TableState::default()
                });
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.in_header = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => self.inline.clear(),
            Tag::Emphasis => self.style_stack.push(SpanStyle {
                italic: true,
                ..SpanStyle::default()
            }),
            Tag::Strong => self.style_stack.push(SpanStyle {
                bold: true,
                ..SpanStyle::default()
            }),
            Tag::Strikethrough => self.style_stack.push(SpanStyle {
                strike: true,
                ..SpanStyle::default()
            }),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                self.style_stack.push(SpanStyle {
                    tone: Some(Tone::Link),
                    underline: true,
                    ..SpanStyle::default()
                });
                let show_destination = !matches!(
                    link_type,
                    LinkType::WikiLink { .. } | LinkType::Autolink | LinkType::Email
                );
                self.link_stack
                    .push(show_destination.then(|| dest_url.to_string()));
            }
            Tag::Image { dest_url, .. } => {
                self.style_stack.push(SpanStyle::muted());
                self.push_span(StyledSpan::new("[image: ", SpanStyle::muted()));
                self.link_stack.push(Some(dest_url.to_string()));
            }
            Tag::MetadataBlock(_) => {
                self.flush_paragraph();
                self.metadata = Some(String::new());
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.resolve_callout_probe();
                if self.callout == Some(CalloutState::Title) {
                    self.finish_callout_title();
                }
                self.flush_paragraph();
                self.pending_blank = true;
            }
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                let spans = std::mem::take(&mut self.inline);
                self.emit_heading(level, spans);
                self.pending_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                if self.callout == Some(CalloutState::Title) {
                    self.finish_callout_title();
                }
                self.flush_paragraph();
                self.callout = None;
                self.containers.pop();
                self.pending_blank = true;
            }
            TagEnd::CodeBlock => {
                if let Some((language, body)) = self.code_block.take() {
                    self.emit_code_block(&language, &body);
                }
                self.pending_blank = true;
            }
            TagEnd::List(_) => {
                self.flush_paragraph();
                self.lists.pop();
                if !self
                    .containers
                    .iter()
                    .any(|container| container.kind == ContainerKind::Item)
                {
                    self.pending_blank = true;
                }
            }
            TagEnd::Item => {
                self.flush_paragraph();
                if self.pending_item_marker.take().is_some() {
                    self.style_stack.pop();
                }
                if let Some(container) = self.containers.pop() {
                    if !container.used {
                        // An empty item still shows its marker.
                        let line = self.prefix_for_line(&container);
                        self.lines.push(line);
                    }
                }
            }
            TagEnd::FootnoteDefinition => {
                self.flush_paragraph();
                self.containers.pop();
                self.pending_blank = true;
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let lines = self.table_lines(&table);
                    self.emit_block_lines(lines);
                }
                self.pending_blank = true;
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    if !table.current_row.is_empty() {
                        table.header = std::mem::take(&mut table.current_row);
                    }
                    table.in_header = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    if table.in_header {
                        table.header = std::mem::take(&mut table.current_row);
                    } else {
                        table.rows.push(std::mem::take(&mut table.current_row));
                    }
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                if let Some(table) = self.table.as_mut() {
                    table.current_row.push(trim_spans(cell));
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.style_stack.pop();
            }
            TagEnd::Link => {
                self.style_stack.pop();
                if let Some(Some(destination)) = self.link_stack.pop() {
                    let text = spans_text(&self.inline);
                    if !destination.is_empty() && !text.ends_with(destination.as_str()) {
                        self.push_span(StyledSpan::new(
                            format!(" ({destination})"),
                            SpanStyle::muted(),
                        ));
                    }
                }
            }
            TagEnd::Image => {
                self.style_stack.pop();
                let destination = self.link_stack.pop().flatten().unwrap_or_default();
                let alt_is_empty = self
                    .inline
                    .last()
                    .is_some_and(|span| span.text == "[image: ");
                let suffix = if alt_is_empty {
                    format!("{destination}]")
                } else {
                    "]".to_string()
                };
                self.push_span(StyledSpan::new(suffix, SpanStyle::muted()));
            }
            TagEnd::MetadataBlock(_) => {
                if let Some(metadata) = self.metadata.take() {
                    let body = metadata.trim_end();
                    if !body.is_empty() {
                        let mut lines = vec![vec![StyledSpan::new("---", SpanStyle::muted())]];
                        lines.extend(
                            body.lines()
                                .map(|line| vec![StyledSpan::new(line, SpanStyle::muted())]),
                        );
                        lines.push(vec![StyledSpan::new("---", SpanStyle::muted())]);
                        self.emit_block_lines(lines);
                        self.pending_blank = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.callout == Some(CalloutState::Detect) {
            // The parser splits `[!kind]` into several text events, so the
            // marker is probed across them.
            self.callout_probe.push_str(text);
            let probe = self.callout_probe.as_str();
            let incomplete = if probe.len() < 2 {
                "[!".starts_with(probe)
            } else {
                probe.starts_with("[!") && !probe.contains(']')
            };
            if incomplete {
                return;
            }
            let probe = std::mem::take(&mut self.callout_probe);
            self.callout = None;
            if let Some((kind, rest)) = parse_callout_marker(&probe) {
                self.begin_callout(&kind);
                self.callout = Some(CalloutState::Title);
                if !rest.is_empty() {
                    self.push_text(&rest);
                }
            } else {
                self.push_text(&probe);
            }
            return;
        }
        let style = self.current_style();
        self.push_span(StyledSpan::new(text, style));
    }

    /// Give up on callout detection, keeping any probed text.
    fn resolve_callout_probe(&mut self) {
        if self.callout == Some(CalloutState::Detect) {
            self.callout = None;
            let probe = std::mem::take(&mut self.callout_probe);
            if !probe.is_empty() {
                self.push_text(&probe);
            }
        }
    }

    fn push_span(&mut self, span: StyledSpan) {
        if self.callout == Some(CalloutState::Title) {
            self.callout_title.push(span);
            return;
        }
        self.resolve_callout_probe();
        if let Some(last) = self.inline.last_mut() {
            if last.style == span.style && span.text != "\n" && last.text != "\n" {
                last.text.push_str(&span.text);
                return;
            }
        }
        self.inline.push(span);
    }

    fn begin_callout(&mut self, kind: &str) {
        self.callout_kind = kind.to_ascii_lowercase();
        self.callout_title.clear();
        let tone = callout_tone(&self.callout_kind);
        if let Some(container) = self.containers.last_mut() {
            let bar = vec![StyledSpan::new(
                "┃ ",
                SpanStyle {
                    tone: Some(tone),
                    ..SpanStyle::default()
                },
            )];
            container.first.clone_from(&bar);
            container.rest = bar;
            container.blank = vec![StyledSpan::new("┃", SpanStyle::tone(tone))];
        }
    }

    fn finish_callout_title(&mut self) {
        self.callout = None;
        let tone = callout_tone(&self.callout_kind);
        let label_style = SpanStyle {
            tone: Some(tone),
            bold: true,
            ..SpanStyle::default()
        };
        let title = trim_spans(std::mem::take(&mut self.callout_title));
        let mut line = vec![StyledSpan::new(
            format!(
                "{} {}",
                callout_icon(&self.callout_kind),
                callout_label(&self.callout_kind)
            ),
            label_style,
        )];
        if !title.is_empty() {
            line.push(StyledSpan::new(": ", label_style));
            line.extend(
                title
                    .into_iter()
                    .map(|span| StyledSpan::new(span.text, span.style.merge(label_style))),
            );
        }
        let saved = std::mem::replace(&mut self.inline, line);
        self.flush_paragraph();
        self.inline = saved;
    }

    fn next_list_marker(&mut self) -> String {
        let depth = self.lists.len();
        let Some(list) = self.lists.last_mut() else {
            return "•".to_string();
        };
        if let Some(index) = list.next_index.as_mut() {
            let marker = format!("{index}.");
            *index += 1;
            marker
        } else if self.options.styled {
            match depth {
                1 => "•",
                2 => "◦",
                _ => "▪",
            }
            .to_string()
        } else {
            "-".to_string()
        }
    }

    fn available_width(&self) -> Option<usize> {
        let prefix_width: usize = self
            .containers
            .iter()
            .map(|container| line_width(&container.rest))
            .sum();
        self.options
            .width
            .map(|width| width.saturating_sub(prefix_width).max(10))
    }

    fn prefix_for_line(&self, last: &Container) -> StyledLine {
        let mut prefix = Vec::new();
        for container in &self.containers {
            prefix.extend(if container.used {
                container.rest.iter().cloned()
            } else {
                container.first.iter().cloned()
            });
        }
        prefix.extend(last.first.iter().cloned());
        prefix
    }

    fn take_prefix(&mut self) -> StyledLine {
        let mut prefix = Vec::new();
        for container in &mut self.containers {
            prefix.extend(if container.used {
                container.rest.iter().cloned()
            } else {
                container.first.iter().cloned()
            });
            container.used = true;
        }
        prefix
    }

    fn emit_blank_if_pending(&mut self) {
        if !self.pending_blank {
            return;
        }
        self.pending_blank = false;
        if self.lines.is_empty() || self.lines.last().is_some_and(|line| line_width(line) == 0) {
            return;
        }
        let blank = self
            .containers
            .iter()
            .flat_map(|container| container.blank.iter().cloned())
            .collect::<Vec<_>>();
        self.lines.push(blank);
    }

    /// Emit pre-laid-out lines inside the current containers.
    fn emit_block_lines(&mut self, lines: Vec<StyledLine>) {
        if lines.is_empty() {
            return;
        }
        self.emit_blank_if_pending();
        for line in lines {
            let mut prefixed = self.take_prefix();
            prefixed.extend(line);
            self.lines.push(prefixed);
        }
    }

    fn flush_paragraph(&mut self) {
        if self.callout == Some(CalloutState::Detect) && !self.callout_probe.is_empty() {
            self.resolve_callout_probe();
        }
        let spans = trim_spans(std::mem::take(&mut self.inline));
        if spans.is_empty() || self.table.is_some() {
            return;
        }
        let lines = wrap_spans(&spans, self.available_width());
        self.emit_block_lines(lines);
    }

    fn emit_heading(&mut self, level: u8, spans: Vec<StyledSpan>) {
        let spans = trim_spans(spans);
        if spans.is_empty() {
            return;
        }
        let heading_style = SpanStyle {
            tone: Some(Tone::Heading),
            bold: true,
            underline: level == 1,
            ..SpanStyle::default()
        };
        let styled = spans
            .into_iter()
            .map(|span| StyledSpan::new(span.text, span.style.merge(heading_style)))
            .collect::<Vec<_>>();
        let mut lines = wrap_spans(&styled, self.available_width());
        if !self.options.styled && level <= 2 {
            let width = lines.iter().map(|line| line_width(line)).max().unwrap_or(0);
            let rule = if level == 1 { "=" } else { "-" };
            lines.push(vec![StyledSpan::new(
                rule.repeat(width),
                SpanStyle::default(),
            )]);
        }
        self.emit_block_lines(lines);
    }

    fn emit_code_block(&mut self, language: &str, body: &str) {
        let body = body.trim_end_matches('\n');
        let mut lines = Vec::new();
        let rule_style = SpanStyle::muted();
        if self.options.styled {
            let label = if language.is_empty() {
                String::new()
            } else {
                format!(" {language} ")
            };
            lines.push(vec![StyledSpan::new(format!("╭─{label}"), rule_style)]);
            for line in body.lines() {
                lines.push(vec![
                    StyledSpan::new("│ ", rule_style),
                    StyledSpan::new(line, SpanStyle::tone(Tone::Code)),
                ]);
            }
            lines.push(vec![StyledSpan::new("╰─", rule_style)]);
        } else {
            lines.push(vec![StyledSpan::new(
                format!("```{language}"),
                SpanStyle::default(),
            )]);
            lines.extend(
                body.lines()
                    .map(|line| vec![StyledSpan::new(line, SpanStyle::default())]),
            );
            lines.push(vec![StyledSpan::new("```", SpanStyle::default())]);
        }
        self.emit_block_lines(lines);
    }

    fn table_lines(&self, table: &TableState) -> Vec<StyledLine> {
        let column_count = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        if column_count == 0 {
            return Vec::new();
        }
        let cell_width = |cell: Option<&Vec<StyledSpan>>| cell.map_or(0, |cell| line_width(cell));
        let mut widths = (0..column_count)
            .map(|index| {
                table
                    .rows
                    .iter()
                    .map(|row| cell_width(row.get(index)))
                    .fold(cell_width(table.header.get(index)), usize::max)
                    .max(1)
            })
            .collect::<Vec<_>>();
        if let Some(available) = self.available_width() {
            shrink_columns(&mut widths, available.saturating_sub(3 * column_count + 1));
        }

        let border = SpanStyle::muted();
        let header_style = SpanStyle {
            bold: true,
            ..SpanStyle::default()
        };
        let render_row = |row: &[Vec<StyledSpan>], style: SpanStyle| {
            let mut line = vec![StyledSpan::new("│ ", border)];
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).cloned().unwrap_or_default();
                let cell = truncate_spans(&cell, *width)
                    .into_iter()
                    .map(|span| StyledSpan::new(span.text, span.style.merge(style)))
                    .collect::<Vec<_>>();
                let padding = width.saturating_sub(line_width(&cell));
                let (left, right) = match table.alignments.get(index) {
                    Some(Alignment::Right) => (padding, 0),
                    Some(Alignment::Center) => (padding / 2, padding - padding / 2),
                    _ => (0, padding),
                };
                line.push(StyledSpan::new(" ".repeat(left), SpanStyle::default()));
                line.extend(cell);
                line.push(StyledSpan::new(" ".repeat(right), SpanStyle::default()));
                line.push(StyledSpan::new(
                    if index + 1 == widths.len() {
                        " │"
                    } else {
                        " │ "
                    },
                    border,
                ));
            }
            line
        };
        let rule = |left: &str, middle: &str, right: &str| {
            let body = widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join(middle);
            vec![StyledSpan::new(format!("{left}{body}{right}"), border)]
        };

        let mut lines = vec![rule("┌", "┬", "┐")];
        if !table.header.is_empty() {
            lines.push(render_row(&table.header, header_style));
            lines.push(rule("├", "┼", "┤"));
        }
        lines.extend(
            table
                .rows
                .iter()
                .map(|row| render_row(row, SpanStyle::default())),
        );
        lines.push(rule("└", "┴", "┘"));
        lines
    }

    fn finish(mut self) -> Vec<StyledLine> {
        if self.callout == Some(CalloutState::Title) {
            self.finish_callout_title();
        }
        self.flush_paragraph();
        while self.lines.last().is_some_and(|line| line_width(line) == 0) {
            self.lines.pop();
        }
        self.lines
    }
}

/// Shrink the widest columns until the table fits in `available` columns.
fn shrink_columns(widths: &mut [usize], available: usize) {
    const MIN_COLUMN: usize = 4;
    while widths.iter().sum::<usize>() > available {
        let Some((index, widest)) = widths
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|(_, width)| *width)
        else {
            return;
        };
        if widest <= MIN_COLUMN {
            return;
        }
        widths[index] = widest - 1;
    }
}

fn truncate_spans(spans: &[StyledSpan], width: usize) -> Vec<StyledSpan> {
    if line_width(spans) <= width {
        return spans.to_vec();
    }
    let mut remaining = width.saturating_sub(1);
    let mut truncated = Vec::new();
    for span in spans {
        let mut text = String::new();
        for ch in span.text.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if ch_width > remaining {
                break;
            }
            remaining -= ch_width;
            text.push(ch);
        }
        let done = text.len() < span.text.len();
        truncated.push(StyledSpan::new(text, span.style));
        if done {
            break;
        }
    }
    truncated.push(StyledSpan::new("…", SpanStyle::muted()));
    truncated
}

fn spans_text(spans: &[StyledSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn trim_spans(mut spans: Vec<StyledSpan>) -> Vec<StyledSpan> {
    while spans
        .first()
        .is_some_and(|span| span.text.trim().is_empty())
    {
        spans.remove(0);
    }
    while spans.last().is_some_and(|span| span.text.trim().is_empty()) {
        spans.pop();
    }
    if let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    spans
}

/// Greedy word wrap that keeps span styles and honours hard breaks.
fn wrap_spans(spans: &[StyledSpan], width: Option<usize>) -> Vec<StyledLine> {
    let mut lines = vec![Vec::new()];
    let mut current_width = 0_usize;
    let push_piece = |line: &mut StyledLine, text: &str, style: SpanStyle| {
        if let Some(last) = line.last_mut() {
            if last.style == style {
                last.text.push_str(text);
                return;
            }
        }
        line.push(StyledSpan::new(text, style));
    };

    for span in spans {
        if span.text == "\n" {
            lines.push(Vec::new());
            current_width = 0;
            continue;
        }
        let Some(width) = width else {
            let line = lines.last_mut().expect("at least one line");
            push_piece(line, &span.text, span.style);
            continue;
        };
        for piece in split_keeping_spaces(&span.text) {
            let piece_width = text_width(piece);
            let is_space = piece.chars().all(char::is_whitespace);
            if is_space {
                if current_width > 0 && current_width + piece_width <= width {
                    let line = lines.last_mut().expect("at least one line");
                    push_piece(line, piece, span.style);
                    current_width += piece_width;
                }
                continue;
            }
            if current_width > 0 && current_width + piece_width > width {
                // Drop the trailing space before breaking.
                if let Some(line) = lines.last_mut() {
                    if let Some(last) = line.last_mut() {
                        let trimmed = last.text.trim_end().len();
                        last.text.truncate(trimmed);
                    }
                }
                lines.push(Vec::new());
                current_width = 0;
            }
            if piece_width <= width {
                let line = lines.last_mut().expect("at least one line");
                push_piece(line, piece, span.style);
                current_width += piece_width;
                continue;
            }
            // A word longer than the line is split at the width.
            let mut chunk = String::new();
            for ch in piece.chars() {
                let ch_width = ch.width().unwrap_or(0);
                if current_width + ch_width > width && current_width > 0 {
                    let line = lines.last_mut().expect("at least one line");
                    push_piece(line, &chunk, span.style);
                    chunk.clear();
                    lines.push(Vec::new());
                    current_width = 0;
                }
                chunk.push(ch);
                current_width += ch_width;
            }
            if !chunk.is_empty() {
                let line = lines.last_mut().expect("at least one line");
                push_piece(line, &chunk, span.style);
            }
        }
    }
    lines
}

fn split_keeping_spaces(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut in_space = None;
    for (index, ch) in text.char_indices() {
        let space = ch.is_whitespace();
        match in_space {
            Some(previous) if previous != space => {
                pieces.push(&text[start..index]);
                start = index;
            }
            _ => {}
        }
        in_space = Some(space);
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

/// Split an Obsidian callout marker such as `[!warning]- Title`.
fn parse_callout_marker(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = &rest[..end];
    if kind.is_empty()
        || !kind
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    let title = rest[end + 1..].trim_start_matches(['+', '-']).trim_start();
    Some((kind.to_string(), title.to_string()))
}

fn gfm_alert_name(kind: BlockQuoteKind) -> &'static str {
    match kind {
        BlockQuoteKind::Note => "note",
        BlockQuoteKind::Tip => "tip",
        BlockQuoteKind::Important => "important",
        BlockQuoteKind::Warning => "warning",
        BlockQuoteKind::Caution => "caution",
    }
}

fn callout_tone(kind: &str) -> Tone {
    match kind {
        "tip" | "hint" | "success" | "check" | "done" => Tone::Success,
        "warning" | "caution" | "attention" | "question" | "help" | "faq" => Tone::Warning,
        "danger" | "error" | "failure" | "fail" | "missing" | "bug" => Tone::Danger,
        "quote" | "cite" | "example" => Tone::Muted,
        _ => Tone::Info,
    }
}

fn callout_icon(kind: &str) -> &'static str {
    match callout_tone(kind) {
        Tone::Success => "✓",
        Tone::Warning => "!",
        Tone::Danger => "✗",
        Tone::Muted => "“",
        _ => "i",
    }
}

fn callout_label(kind: &str) -> String {
    let mut chars = kind.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(markdown: &str) -> String {
        render_terminal_markdown(markdown, false)
    }

    fn plain_width(markdown: &str, width: usize) -> String {
        render_terminal_markdown_with(
            markdown,
            TerminalMarkdownOptions {
                width: Some(width),
                styled: false,
            },
        )
    }

    #[test]
    fn renders_markdown_without_raw_heading_markers() {
        let rendered = plain(
            "# Title\n\nParagraph with `code`.\n\n- one\n- two\n\n```js\nconst value = 1;\n```",
        );
        assert_eq!(
            rendered,
            "Title\n=====\n\nParagraph with `code`.\n\n- one\n- two\n\n```js\nconst value = 1;\n```"
        );
    }

    #[test]
    fn nested_lists_keep_their_own_lines_and_numbering() {
        assert_eq!(
            plain("1. first\n   - nested\n2. second\n"),
            "1. first\n   - nested\n2. second"
        );
        assert_eq!(
            plain("- [ ] Write docs\n  - [x] Ship release\n"),
            "[ ] Write docs\n    [x] Ship release"
        );
    }

    #[test]
    fn list_items_wrap_with_a_hanging_indent() {
        assert_eq!(
            plain_width("- alpha beta gamma delta\n", 14),
            "- alpha beta\n  gamma delta"
        );
    }

    #[test]
    fn paragraphs_wrap_to_the_width() {
        assert_eq!(
            plain_width("one two three four five six", 12),
            "one two\nthree four\nfive six"
        );
    }

    #[test]
    fn renders_tables_as_aligned_boxes() {
        let rendered = plain("| Name | Hours |\n| --- | ---: |\n| Alpha | 2 |\n| Beta | 12 |");
        assert_eq!(
            rendered,
            concat!(
                "┌───────┬───────┐\n",
                "│ Name  │ Hours │\n",
                "├───────┼───────┤\n",
                "│ Alpha │     2 │\n",
                "│ Beta  │    12 │\n",
                "└───────┴───────┘"
            )
        );
    }

    #[test]
    fn wide_tables_shrink_to_the_width() {
        let rendered = plain_width("| A | B |\n| --- | --- |\n| abcdefghijkl | x |", 16);
        assert!(
            rendered.lines().all(|line| text_width(line) <= 16),
            "{rendered}"
        );
        assert!(rendered.contains('…'), "{rendered}");
    }

    #[test]
    fn separates_quotes_from_following_lists() {
        let rendered = plain("> quoted\n\n1. first\n2. second");
        assert_eq!(rendered, "│ quoted\n\n1. first\n2. second");
    }

    #[test]
    fn renders_callouts_with_their_title() {
        assert_eq!(
            plain("> [!warning] Careful\n> Body text\n"),
            "┃ ! Warning: Careful\n┃ Body text"
        );
        assert_eq!(plain("> [!NOTE]\n> GFM alert\n"), "┃ i Note\n┃ GFM alert");
    }

    #[test]
    fn keeps_source_characters_and_shows_link_targets() {
        assert_eq!(
            plain("choices:: \"alpha\" [[Projects/Alpha|Alpha]] [site](https://x.test)"),
            "choices:: \"alpha\" Alpha site (https://x.test)"
        );
    }

    #[test]
    fn renders_yaml_frontmatter_before_body_content() {
        let rendered = plain("---\ntitle: Alpha\ntags:\n  - project\n---\n\n# Heading\n\nBody\n");
        assert!(rendered.starts_with("---\ntitle: Alpha\ntags:\n  - project\n---\n\nHeading"));
        assert!(rendered.contains("Body"));
    }

    #[test]
    fn styled_output_uses_ansi_attributes() {
        let rendered = render_terminal_markdown("**bold** and `code`", true);
        assert_eq!(
            rendered,
            "\u{1b}[1mbold\u{1b}[0m and \u{1b}[33mcode\u{1b}[0m"
        );
    }
}
