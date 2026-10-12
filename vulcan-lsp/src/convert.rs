//! Conversions between LSP wire types and Vulcan's editor types.

use lsp_types as lsp;
use std::path::{Path, PathBuf};
use vulcan_app::editor::{
    EditorCompletionKind, EditorDiagnostic, EditorDiagnosticSeverity, EditorRange, EditorSymbol,
};
use vulcan_core::editor::{PositionEncoding, TextPosition};

pub(crate) fn file_path(uri: &lsp::Uri) -> Option<PathBuf> {
    let parsed = url::Url::parse(uri.as_str()).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed.to_file_path().ok()
}

pub(crate) fn file_uri(path: &Path) -> Option<lsp::Uri> {
    let url = url::Url::from_file_path(path).ok()?;
    url.as_str().parse().ok()
}

pub(crate) fn position(position: lsp::Position) -> TextPosition {
    TextPosition {
        line: position.line,
        column: position.character,
    }
}

pub(crate) fn lsp_position(position: TextPosition) -> lsp::Position {
    lsp::Position {
        line: position.line,
        character: position.column,
    }
}

pub(crate) fn range(range: lsp::Range) -> EditorRange {
    EditorRange {
        start: position(range.start),
        end: position(range.end),
    }
}

pub(crate) fn lsp_range(range: EditorRange) -> lsp::Range {
    lsp::Range {
        start: lsp_position(range.start),
        end: lsp_position(range.end),
    }
}

/// Pick UTF-8 when the client offers it, otherwise the mandatory UTF-16.
pub(crate) fn negotiate_encoding(
    capabilities: &lsp::ClientCapabilities,
) -> (PositionEncoding, lsp::PositionEncodingKind) {
    let offered = capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref());
    if offered.is_some_and(|kinds| kinds.contains(&lsp::PositionEncodingKind::UTF8)) {
        (PositionEncoding::Utf8, lsp::PositionEncodingKind::UTF8)
    } else {
        (PositionEncoding::Utf16, lsp::PositionEncodingKind::UTF16)
    }
}

pub(crate) fn diagnostic(diagnostic: EditorDiagnostic) -> lsp::Diagnostic {
    let severity = match diagnostic.severity {
        EditorDiagnosticSeverity::Error => lsp::DiagnosticSeverity::ERROR,
        EditorDiagnosticSeverity::Warning => lsp::DiagnosticSeverity::WARNING,
        EditorDiagnosticSeverity::Information => lsp::DiagnosticSeverity::INFORMATION,
        EditorDiagnosticSeverity::Hint => lsp::DiagnosticSeverity::HINT,
    };
    lsp::Diagnostic {
        range: lsp_range(diagnostic.range),
        severity: Some(severity),
        code: Some(lsp::NumberOrString::String(diagnostic.code.to_string())),
        source: Some("vulcan".to_string()),
        message: diagnostic.message,
        data: (!diagnostic.candidates.is_empty())
            .then(|| serde_json::json!({ "candidates": diagnostic.candidates })),
        ..lsp::Diagnostic::default()
    }
}

pub(crate) fn completion_kind(kind: EditorCompletionKind) -> lsp::CompletionItemKind {
    match kind {
        EditorCompletionKind::Note | EditorCompletionKind::Attachment => {
            lsp::CompletionItemKind::FILE
        }
        EditorCompletionKind::Alias | EditorCompletionKind::Block => {
            lsp::CompletionItemKind::REFERENCE
        }
        EditorCompletionKind::Heading => lsp::CompletionItemKind::MODULE,
        EditorCompletionKind::Tag => lsp::CompletionItemKind::KEYWORD,
        EditorCompletionKind::Property => lsp::CompletionItemKind::PROPERTY,
    }
}

#[allow(deprecated)] // `DocumentSymbol::deprecated` must still be set.
pub(crate) fn document_symbol(symbol: EditorSymbol) -> lsp::DocumentSymbol {
    lsp::DocumentSymbol {
        name: symbol.name,
        detail: Some(symbol.section_id),
        kind: lsp::SymbolKind::STRING,
        tags: None,
        deprecated: None,
        range: lsp_range(symbol.range),
        selection_range: lsp_range(symbol.selection_range),
        children: Some(symbol.children.into_iter().map(document_symbol).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_file_uris_with_escapes() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\vault\My Notes\ü.md")
        } else {
            PathBuf::from("/vault/My Notes/ü.md")
        };
        let uri = file_uri(&path).unwrap();
        assert!(uri.as_str().contains("My%20Notes"));
        assert_eq!(file_path(&uri), Some(path));
        let other: lsp::Uri = "untitled:Untitled-1".parse().unwrap();
        assert_eq!(file_path(&other), None);
    }

    #[test]
    fn prefers_utf8_when_offered() {
        let mut capabilities = lsp::ClientCapabilities::default();
        assert_eq!(negotiate_encoding(&capabilities).0, PositionEncoding::Utf16);
        capabilities.general = Some(lsp::GeneralClientCapabilities {
            position_encodings: Some(vec![
                lsp::PositionEncodingKind::UTF16,
                lsp::PositionEncodingKind::UTF8,
            ]),
            ..lsp::GeneralClientCapabilities::default()
        });
        assert_eq!(negotiate_encoding(&capabilities).0, PositionEncoding::Utf8);
    }
}
