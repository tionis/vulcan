use crate::config::{ChunkingConfig, ChunkingStrategy};
use crate::parser::types::{ChunkText, SemanticBlock};

pub const CHUNK_VERSION: u32 = 1;

pub(crate) fn chunk_blocks(blocks: &[SemanticBlock], config: &ChunkingConfig) -> Vec<ChunkText> {
    match config.strategy {
        ChunkingStrategy::Heading => chunk_by_heading(blocks, config),
        ChunkingStrategy::Fixed => chunk_fixed(blocks, config),
        ChunkingStrategy::Paragraph => chunk_paragraphs(blocks, config),
    }
}

fn chunk_by_heading(blocks: &[SemanticBlock], config: &ChunkingConfig) -> Vec<ChunkText> {
    let mut chunks = Vec::new();
    let mut builder: Option<ChunkBuilder> = None;

    for block in blocks {
        let heading_changed = builder
            .as_ref()
            .is_some_and(|current| current.heading_path != block.heading_path);
        let would_overflow = builder.as_ref().is_some_and(|current| {
            !current.is_empty()
                && current.heading_path == block.heading_path
                && current.content_len_with_block(&block.text) > config.target_size
        });

        if heading_changed || would_overflow {
            if let Some(current) = builder.take() {
                chunks.push(current.build(chunks.len(), "heading"));
            }
        }

        let current = builder.get_or_insert_with(|| {
            let mut current =
                ChunkBuilder::new(block.heading_path.clone(), block.byte_offset_start);
            if let Some(heading) = block.heading_path.last() {
                current.push_text(heading);
                current.push_text("\n\n");
            }
            current
        });
        current.push_block(block);
    }

    if let Some(current) = builder {
        chunks.push(current.build(chunks.len(), "heading"));
    }

    chunks
}

fn chunk_fixed(blocks: &[SemanticBlock], config: &ChunkingConfig) -> Vec<ChunkText> {
    let mut chunks = Vec::new();
    let mut builder: Option<ChunkBuilder> = None;
    let mut overlap_prefix = String::new();

    for block in blocks {
        let would_overflow = builder.as_ref().is_some_and(|current| {
            !current.is_empty() && current.content_len_with_block(&block.text) > config.target_size
        });

        if would_overflow {
            if let Some(current) = builder.take() {
                overlap_prefix = overlap_suffix(&current.content, config.overlap);
                chunks.push(current.build(chunks.len(), "fixed"));
            }
        }

        let current = builder.get_or_insert_with(|| {
            let mut current =
                ChunkBuilder::new(block.heading_path.clone(), block.byte_offset_start);
            if !overlap_prefix.is_empty() {
                current.push_text(&overlap_prefix);
                current.push_text("\n");
            }
            current
        });
        current.push_block(block);
    }

    if let Some(current) = builder {
        chunks.push(current.build(chunks.len(), "fixed"));
    }

    chunks
}

fn chunk_paragraphs(blocks: &[SemanticBlock], _config: &ChunkingConfig) -> Vec<ChunkText> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            ChunkText::new(
                block.text.clone(),
                index,
                block.heading_path.clone(),
                block.byte_offset_start,
                block.byte_offset_end,
                strategy_name(ChunkingStrategy::Paragraph).to_string(),
                CHUNK_VERSION,
            )
        })
        .collect()
}

fn overlap_suffix(content: &str, overlap: usize) -> String {
    if overlap == 0 {
        return String::new();
    }

    content
        .chars()
        .rev()
        .take(overlap)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn strategy_name(strategy: ChunkingStrategy) -> &'static str {
    match strategy {
        ChunkingStrategy::Heading => "heading",
        ChunkingStrategy::Fixed => "fixed",
        ChunkingStrategy::Paragraph => "paragraph",
    }
}

#[derive(Debug, Clone)]
struct ChunkBuilder {
    content: String,
    heading_path: Vec<String>,
    byte_offset_start: usize,
    byte_offset_end: usize,
}

impl ChunkBuilder {
    fn new(heading_path: Vec<String>, byte_offset_start: usize) -> Self {
        Self {
            content: String::new(),
            heading_path,
            byte_offset_start,
            byte_offset_end: byte_offset_start,
        }
    }

    fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }

    fn content_len_with_block(&self, block_text: &str) -> usize {
        self.content.chars().count()
            + separator_len(self.content.is_empty())
            + block_text.chars().count()
    }

    fn push_text(&mut self, text: &str) {
        self.content.push_str(text);
    }

    fn push_block(&mut self, block: &SemanticBlock) {
        if !self.content.is_empty() && !self.content.ends_with("\n\n") {
            self.content.push_str("\n\n");
        }
        self.content.push_str(&block.text);
        self.byte_offset_end = block.byte_offset_end;
    }

    fn build(self, sequence_index: usize, strategy_name: &str) -> ChunkText {
        ChunkText::new(
            self.content,
            sequence_index,
            self.heading_path,
            self.byte_offset_start,
            self.byte_offset_end,
            strategy_name.to_string(),
            CHUNK_VERSION,
        )
    }
}

fn separator_len(is_empty: bool) -> usize {
    if is_empty {
        0
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChunkingConfig, ChunkingStrategy};
    use crate::parser::types::{SemanticBlock, SemanticBlockKind};

    #[test]
    fn heading_strategy_splits_on_heading_boundaries() {
        let blocks = vec![
            block("intro", 0..5, &[]),
            block("alpha body", 10..20, &["Alpha"]),
            block("beta body", 30..40, &["Beta"]),
        ];

        let chunks = chunk_blocks(&blocks, &ChunkingConfig::default());

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].heading_path, vec!["Alpha".to_string()]);
        assert!(chunks[1].content.starts_with("Alpha\n\nalpha body"));
        assert_eq!(chunks[2].heading_path, vec!["Beta".to_string()]);
    }

    #[test]
    fn oversized_single_block_stays_intact() {
        let config = ChunkingConfig {
            target_size: 4,
            ..ChunkingConfig::default()
        };
        let blocks = vec![block("this block is long", 0..17, &["Alpha"])];

        let chunks = chunk_blocks(&blocks, &config);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("this block is long"));
    }

    #[test]
    fn paragraph_strategy_emits_one_chunk_per_block() {
        let config = ChunkingConfig {
            strategy: ChunkingStrategy::Paragraph,
            ..ChunkingConfig::default()
        };
        let blocks = vec![block("one", 0..3, &[]), block("two", 4..7, &["Section"])];

        let chunks = chunk_blocks(&blocks, &config);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].content, "one");
        assert_eq!(chunks[1].content, "two");
    }

    #[test]
    fn fixed_strategy_is_deterministic() {
        let config = ChunkingConfig {
            strategy: ChunkingStrategy::Fixed,
            target_size: 8,
            overlap: 2,
        };
        let blocks = vec![block("alpha", 0..5, &[]), block("beta", 6..10, &[])];

        let first = chunk_blocks(&blocks, &config);
        let second = chunk_blocks(&blocks, &config);

        assert_eq!(first, second);
    }

    #[test]
    fn empty_input_produces_no_chunks() {
        assert!(chunk_blocks(&[], &ChunkingConfig::default()).is_empty());
    }

    fn block(text: &str, range: std::ops::Range<usize>, heading_path: &[&str]) -> SemanticBlock {
        SemanticBlock {
            block_kind: SemanticBlockKind::Paragraph,
            text: text.to_string(),
            byte_offset_start: range.start,
            byte_offset_end: range.end,
            heading_path: heading_path
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            code_language: None,
        }
    }
}
