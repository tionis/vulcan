use serde_yaml::Value;
use std::ops::Range;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedDocument {
    pub raw_frontmatter: Option<String>,
    pub frontmatter: Option<Value>,
    pub headings: Vec<RawHeading>,
    pub block_refs: Vec<RawBlockRef>,
    pub links: Vec<RawLink>,
    pub tags: Vec<RawTag>,
    pub aliases: Vec<String>,
    pub inline_fields: Vec<RawInlineField>,
    pub list_items: Vec<RawListItem>,
    pub tasks: Vec<RawTask>,
    pub dataview_blocks: Vec<RawDataviewBlock>,
    pub inline_expressions: Vec<RawInlineExpression>,
    pub chunk_texts: Vec<ChunkText>,
    pub diagnostics: Vec<ParseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHeading {
    pub level: u8,
    pub text: String,
    pub byte_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBlockRef {
    pub block_id_text: String,
    pub block_id_byte_offset: usize,
    pub target_block_byte_start: usize,
    pub target_block_byte_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Wikilink,
    Markdown,
    Embed,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginContext {
    Body,
    Frontmatter,
    Property,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLink {
    pub raw_text: String,
    pub link_kind: LinkKind,
    pub display_text: Option<String>,
    pub target_path_candidate: Option<String>,
    pub target_heading: Option<String>,
    pub target_block: Option<String>,
    pub origin_context: OriginContext,
    pub byte_offset: usize,
    pub is_note_embed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTag {
    pub tag_text: String,
    pub byte_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineFieldKind {
    Bare,
    Parenthesized,
    Bracket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawInlineField {
    pub key: String,
    pub value_text: String,
    pub kind: InlineFieldKind,
    pub byte_range: Range<usize>,
    pub value_byte_range: Range<usize>,
    pub line_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTaskField {
    pub key: String,
    pub value_text: String,
    pub kind: InlineFieldKind,
    pub byte_range: Range<usize>,
    pub value_byte_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawListItem {
    pub symbol: String,
    pub text: String,
    pub tags: Vec<String>,
    pub outlinks: Vec<String>,
    pub byte_offset: usize,
    pub parent_item_index: Option<usize>,
    pub section_heading: Option<String>,
    pub line_number: usize,
    pub line_count: usize,
    pub is_task: bool,
    pub block_id: Option<String>,
    pub annotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTask {
    pub list_item_index: usize,
    pub status_char: char,
    pub text: String,
    pub byte_offset: usize,
    pub parent_task_index: Option<usize>,
    pub section_heading: Option<String>,
    pub line_number: usize,
    pub inline_fields: Vec<RawTaskField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDataviewBlock {
    pub language: String,
    pub text: String,
    pub block_index: usize,
    pub byte_range: Range<usize>,
    pub line_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawInlineExpression {
    pub expression: String,
    pub byte_range: Range<usize>,
    pub line_number: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseDiagnosticKind {
    HtmlLink,
    LinkInComment,
    MalformedFrontmatter,
    UnsupportedSyntax,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub kind: ParseDiagnosticKind,
    pub message: String,
    pub byte_range: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkText {
    pub content: String,
    pub sequence_index: usize,
    pub heading_path: Vec<String>,
    pub byte_offset_start: usize,
    pub byte_offset_end: usize,
    pub content_hash: Vec<u8>,
    pub chunk_strategy: String,
    pub chunk_version: u32,
}

impl ChunkText {
    #[must_use]
    pub fn new(
        content: String,
        sequence_index: usize,
        heading_path: Vec<String>,
        byte_offset_start: usize,
        byte_offset_end: usize,
        chunk_strategy: String,
        chunk_version: u32,
    ) -> Self {
        Self {
            content_hash: blake3::hash(content.as_bytes()).as_bytes().to_vec(),
            content,
            sequence_index,
            heading_path,
            byte_offset_start,
            byte_offset_end,
            chunk_strategy,
            chunk_version,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticBlockKind {
    Paragraph,
    BlockQuote,
    CodeBlock,
    HtmlBlock,
    List,
    Table,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticBlock {
    pub block_kind: SemanticBlockKind,
    pub text: String,
    pub byte_offset_start: usize,
    pub byte_offset_end: usize,
    pub heading_path: Vec<String>,
    pub code_language: Option<String>,
}
