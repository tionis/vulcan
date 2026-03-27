use crate::chunking::chunk_blocks;
use crate::config::{AttachmentExtractionConfig, ChunkingConfig, VaultConfig};
use crate::parser::types::{ChunkText, SemanticBlock, SemanticBlockKind};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub(crate) enum AttachmentExtractionError {
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    Io(std::io::Error),
    InvalidUtf8(std::string::FromUtf8Error),
}

impl Display for AttachmentExtractionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommandFailed {
                command,
                status,
                stderr,
            } => {
                if stderr.is_empty() {
                    write!(
                        formatter,
                        "attachment extractor `{command}` failed with {status}"
                    )
                } else {
                    write!(
                        formatter,
                        "attachment extractor `{command}` failed with {status}: {stderr}"
                    )
                }
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidUtf8(error) => write!(
                formatter,
                "attachment extractor output was not valid UTF-8: {error}"
            ),
        }
    }
}

impl Error for AttachmentExtractionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidUtf8(error) => Some(error),
            Self::CommandFailed { .. } => None,
        }
    }
}

impl From<std::io::Error> for AttachmentExtractionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<std::string::FromUtf8Error> for AttachmentExtractionError {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Self::InvalidUtf8(error)
    }
}

pub(crate) fn extract_attachment_chunks(
    config: &VaultConfig,
    absolute_path: &Path,
    relative_path: &str,
) -> Result<Vec<ChunkText>, AttachmentExtractionError> {
    let Some(extraction) = config.extraction.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(extension) = absolute_path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Ok(Vec::new());
    };
    if !extraction.supports_extension(&extension) {
        return Ok(Vec::new());
    }

    let extracted_text = run_extractor(extraction, absolute_path, relative_path, &extension)?;
    let normalized = normalize_extracted_text(&extracted_text, extraction.max_output_bytes());
    if normalized.is_empty() {
        return Ok(Vec::new());
    }

    Ok(chunk_extracted_text(&normalized, &config.chunking))
}

fn run_extractor(
    extraction: &AttachmentExtractionConfig,
    absolute_path: &Path,
    relative_path: &str,
    extension: &str,
) -> Result<String, AttachmentExtractionError> {
    let absolute_path = absolute_path.to_string_lossy().into_owned();
    let mut command = Command::new(&extraction.command);
    for argument in &extraction.args {
        command.arg(
            argument
                .replace("{path}", &absolute_path)
                .replace("{relative_path}", relative_path)
                .replace("{extension}", extension),
        );
    }

    let output = command.output()?;
    if !output.status.success() {
        return Err(AttachmentExtractionError::CommandFailed {
            command: extraction.command.clone(),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| format!("exit code {code}")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    String::from_utf8(output.stdout).map_err(AttachmentExtractionError::from)
}

fn normalize_extracted_text(text: &str, max_output_bytes: usize) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\0', " ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    truncate_utf8(trimmed, max_output_bytes).trim().to_string()
}

fn truncate_utf8(text: &str, max_output_bytes: usize) -> String {
    if text.len() <= max_output_bytes {
        return text.to_string();
    }

    let mut end = 0_usize;
    for (index, character) in text.char_indices() {
        let next = index + character.len_utf8();
        if next > max_output_bytes {
            break;
        }
        end = next;
    }
    text[..end].to_string()
}

fn chunk_extracted_text(text: &str, config: &ChunkingConfig) -> Vec<ChunkText> {
    let mut blocks = Vec::new();
    let mut start = 0_usize;

    for (index, _) in text.match_indices("\n\n") {
        push_extracted_block(&mut blocks, &text[start..index], start);
        start = index + 2;
    }
    push_extracted_block(&mut blocks, &text[start..], start);

    if blocks.is_empty() {
        return vec![ChunkText::new(
            text.to_string(),
            0,
            Vec::new(),
            0,
            text.len(),
            "attachment".to_string(),
            crate::chunking::CHUNK_VERSION,
        )];
    }

    chunk_blocks(&blocks, config)
}

fn push_extracted_block(blocks: &mut Vec<SemanticBlock>, segment: &str, base_offset: usize) {
    let trimmed_start = segment.trim_start();
    let trimmed = trimmed_start.trim_end();
    if trimmed.is_empty() {
        return;
    }

    let leading = segment.len().saturating_sub(trimmed_start.len());
    let trailing = segment.len().saturating_sub(trimmed.len() + leading);
    blocks.push(SemanticBlock {
        block_kind: SemanticBlockKind::Paragraph,
        text: trimmed.to_string(),
        byte_offset_start: base_offset + leading,
        byte_offset_end: base_offset + segment.len().saturating_sub(trailing),
        heading_path: Vec::new(),
        code_language: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AttachmentExtractionConfig, ChunkingStrategy, LinkResolutionMode, LinkStylePreference,
    };
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn extractor_runs_command_and_chunks_attachment_text() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let attachment_path = temp_dir.path().join("guide.pdf");
        fs::write(&attachment_path, "pdf fixture").expect("attachment should write");
        fs::write(
            temp_dir.path().join("guide.pdf.txt"),
            "dashboard manual\n\nBob",
        )
        .expect("sidecar text should write");

        let config = VaultConfig {
            extraction: Some(AttachmentExtractionConfig {
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "cat \"$1.txt\"".to_string(),
                    "sh".to_string(),
                    "{path}".to_string(),
                ],
                extensions: vec!["pdf".to_string()],
                max_output_bytes: Some(1024),
            }),
            chunking: ChunkingConfig {
                strategy: ChunkingStrategy::Paragraph,
                target_size: 128,
                overlap: 0,
            },
            link_resolution: LinkResolutionMode::Shortest,
            link_style: LinkStylePreference::Wikilink,
            attachment_folder: ".".into(),
            strict_line_breaks: false,
            property_types: std::collections::BTreeMap::default(),
            embedding: None,
            ..VaultConfig::default()
        };

        let chunks = extract_attachment_chunks(&config, &attachment_path, "guide.pdf")
            .expect("attachment extraction should succeed");

        assert_eq!(chunks.len(), 2);
        assert!(chunks
            .iter()
            .any(|chunk| chunk.content.contains("dashboard")));
        assert!(chunks.iter().any(|chunk| chunk.content.contains("Bob")));
    }

    #[test]
    fn extractor_skips_disabled_or_unsupported_extensions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let attachment_path = temp_dir.path().join("logo.png");
        fs::write(&attachment_path, "png fixture").expect("attachment should write");

        assert!(
            extract_attachment_chunks(&VaultConfig::default(), &attachment_path, "logo.png")
                .expect("disabled extraction should succeed")
                .is_empty()
        );

        let config = VaultConfig {
            extraction: Some(AttachmentExtractionConfig {
                command: "cat".to_string(),
                args: vec!["{path}".to_string()],
                extensions: vec!["pdf".to_string()],
                max_output_bytes: None,
            }),
            ..VaultConfig::default()
        };
        assert!(
            extract_attachment_chunks(&config, &attachment_path, "logo.png")
                .expect("unsupported extension should succeed")
                .is_empty()
        );
    }
}
