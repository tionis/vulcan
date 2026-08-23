use crate::chunking::chunk_blocks;
use crate::config::{AttachmentExtractionConfig, ChunkingConfig, VaultConfig};
use crate::parser::types::{ChunkText, SemanticBlock, SemanticBlockKind};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EXTRACTION_STDERR_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) enum AttachmentExtractionError {
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    Io(std::io::Error),
    InvalidUtf8(std::string::FromUtf8Error),
    OutputLimitExceeded {
        stream: &'static str,
        limit: usize,
    },
    TimedOut {
        command: String,
    },
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
            Self::OutputLimitExceeded { stream, limit } => write!(
                formatter,
                "attachment extractor {stream} exceeded {limit} byte limit"
            ),
            Self::TimedOut { command } => {
                write!(formatter, "attachment extractor `{command}` timed out")
            }
        }
    }
}

impl Error for AttachmentExtractionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidUtf8(error) => Some(error),
            Self::CommandFailed { .. }
            | Self::OutputLimitExceeded { .. }
            | Self::TimedOut { .. } => None,
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
    run_extractor_with_timeout(
        extraction,
        absolute_path,
        relative_path,
        extension,
        EXTRACTION_TIMEOUT,
    )
}

fn run_extractor_with_timeout(
    extraction: &AttachmentExtractionConfig,
    absolute_path: &Path,
    relative_path: &str,
    extension: &str,
    timeout: Duration,
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

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| {
        AttachmentExtractionError::Io(std::io::Error::other("missing extractor stdout"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AttachmentExtractionError::Io(std::io::Error::other("missing extractor stderr"))
    })?;
    let (limit_tx, limit_rx) = mpsc::channel();
    let stdout_limit = extraction.max_output_bytes();
    let stdout_reader = spawn_bounded_reader(stdout, "stdout", stdout_limit, limit_tx.clone());
    let stderr_reader =
        spawn_bounded_reader(stderr, "stderr", MAX_EXTRACTION_STDERR_BYTES, limit_tx);
    let started = Instant::now();
    let (status, forced_error) = loop {
        if let Ok(error) = limit_rx.try_recv() {
            terminate_extractor_process_tree(&mut child);
            break (child.wait()?, Some(error));
        }
        if started.elapsed() >= timeout {
            terminate_extractor_process_tree(&mut child);
            break (
                child.wait()?,
                Some(AttachmentExtractionError::TimedOut {
                    command: extraction.command.clone(),
                }),
            );
        }
        if let Some(status) = child.try_wait()? {
            break (status, None);
        }
        thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout_reader.join().map_err(|_| {
        AttachmentExtractionError::Io(std::io::Error::other("extractor stdout reader panicked"))
    })??;
    let stderr = stderr_reader.join().map_err(|_| {
        AttachmentExtractionError::Io(std::io::Error::other("extractor stderr reader panicked"))
    })??;
    if let Some(error) = forced_error {
        return Err(error);
    }
    if stdout.exceeded {
        return Err(AttachmentExtractionError::OutputLimitExceeded {
            stream: "stdout",
            limit: stdout_limit,
        });
    }
    if stderr.exceeded {
        return Err(AttachmentExtractionError::OutputLimitExceeded {
            stream: "stderr",
            limit: MAX_EXTRACTION_STDERR_BYTES,
        });
    }
    if !status.success() {
        return Err(AttachmentExtractionError::CommandFailed {
            command: extraction.command.clone(),
            status: status
                .code()
                .map_or_else(|| "signal".to_string(), |code| format!("exit code {code}")),
            stderr: String::from_utf8_lossy(&stderr.bytes).trim().to_string(),
        });
    }

    String::from_utf8(stdout.bytes).map_err(AttachmentExtractionError::from)
}

fn terminate_extractor_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn spawn_bounded_reader(
    mut reader: impl Read + Send + 'static,
    stream: &'static str,
    limit: usize,
    limit_tx: mpsc::Sender<AttachmentExtractionError>,
) -> thread::JoinHandle<Result<BoundedRead, std::io::Error>> {
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(limit.min(8192));
        let mut chunk = [0_u8; 8192];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                return Ok(BoundedRead {
                    bytes,
                    exceeded: false,
                });
            }
            let remaining = limit.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..read.min(remaining)]);
            if read > remaining {
                let _ =
                    limit_tx.send(AttachmentExtractionError::OutputLimitExceeded { stream, limit });
                return Ok(BoundedRead {
                    bytes,
                    exceeded: true,
                });
            }
        }
    })
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

    #[test]
    fn extractor_stops_streaming_output_at_the_configured_limit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let attachment_path = temp_dir.path().join("guide.pdf");
        fs::write(&attachment_path, "pdf fixture").expect("attachment should write");
        let extraction = AttachmentExtractionConfig {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "while :; do printf '0123456789abcdef'; done".to_string(),
            ],
            extensions: vec!["pdf".to_string()],
            max_output_bytes: Some(1024),
        };

        let started = Instant::now();
        let error = run_extractor_with_timeout(
            &extraction,
            &attachment_path,
            "guide.pdf",
            "pdf",
            Duration::from_secs(2),
        )
        .expect_err("unlimited extractor output should be rejected");

        assert!(matches!(
            error,
            AttachmentExtractionError::OutputLimitExceeded {
                stream: "stdout",
                limit: 1024
            }
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn extractor_stops_streaming_stderr_at_the_diagnostic_limit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let attachment_path = temp_dir.path().join("guide.pdf");
        fs::write(&attachment_path, "pdf fixture").expect("attachment should write");
        let extraction = AttachmentExtractionConfig {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "while :; do printf '0123456789abcdef' >&2; done".to_string(),
            ],
            extensions: vec!["pdf".to_string()],
            max_output_bytes: Some(1024),
        };

        let error = run_extractor_with_timeout(
            &extraction,
            &attachment_path,
            "guide.pdf",
            "pdf",
            Duration::from_secs(2),
        )
        .expect_err("unlimited extractor diagnostics should be rejected");

        assert!(matches!(
            error,
            AttachmentExtractionError::OutputLimitExceeded {
                stream: "stderr",
                limit: MAX_EXTRACTION_STDERR_BYTES
            }
        ));
    }

    #[test]
    fn extractor_kills_a_command_that_exceeds_its_deadline() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let attachment_path = temp_dir.path().join("guide.pdf");
        fs::write(&attachment_path, "pdf fixture").expect("attachment should write");
        let extraction = AttachmentExtractionConfig {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "while :; do :; done".to_string()],
            extensions: vec!["pdf".to_string()],
            max_output_bytes: Some(1024),
        };

        let started = Instant::now();
        let error = run_extractor_with_timeout(
            &extraction,
            &attachment_path,
            "guide.pdf",
            "pdf",
            Duration::from_millis(50),
        )
        .expect_err("non-terminating extractor should time out");

        assert!(matches!(error, AttachmentExtractionError::TimedOut { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
