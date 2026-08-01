//! Map the parser's unified diagnostics onto LSP `Diagnostic`s for one open
//! buffer, honestly.
//!
//! A parser [`UnifiedDiagnostic`] carries an optional `CodeLocation` (a byte
//! span into some source file) and, for DFM findings, an optional dfm byte
//! offset. We render an LSP `Range` for a diagnostic ONLY when its location
//! points into the buffer we are analyzing — then the byte span maps exactly
//! through the buffer's [`LineIndex`]. A diagnostic whose only anchor is a DFM
//! offset (no pas span), or whose location points at a DIFFERENT file, must NOT
//! get a fabricated pas range (the never-a-wrong-answer rule, mirroring the
//! parser's own honesty). Such a finding is attached at the top of the document
//! (range 0:0–0:0) — a best-effort unit-level anchor — with its dfm offset noted
//! in the message so the information is not lost.

use delphi_parser::meta::FileId;
use delphi_parser::query::{DiagnosticSource, UnifiedDiagnostic};
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, Position, Range,
};

use crate::positions::LineIndex;

/// Convert one unit's unified diagnostics to LSP diagnostics.
///
/// `buffer_file` is the [`FileId`] of the analyzed buffer; only diagnostics
/// whose location is in that file get an exact byte-mapped range. `index` is the
/// buffer's line index.
pub fn to_lsp_diagnostics(
    diagnostics: &[UnifiedDiagnostic],
    buffer_file: FileId,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| to_lsp(diagnostic, buffer_file, index))
        .collect()
}

fn to_lsp(diagnostic: &UnifiedDiagnostic, buffer_file: FileId, index: &LineIndex) -> Diagnostic {
    let range = match diagnostic.location {
        // A location IN this buffer → exact byte→UTF-16 range.
        Some(location) if location.file == buffer_file => Range {
            start: index.position_of(location.span.start as usize),
            end: index.position_of(location.span.end as usize),
        },
        // A location in a DIFFERENT file (e.g. a dfm finding pointing at an
        // on-disk pas member), or no location at all → do NOT fabricate a pas
        // range for THIS buffer. Anchor at the top of the document.
        _ => top_of_document(),
    };

    let mut message = diagnostic.message.clone();
    // A DFM finding whose only anchor is a dfm offset: surface the offset in the
    // message rather than pretending it maps into the pas buffer.
    if diagnostic.source == DiagnosticSource::Dfm {
        if let Some(offset) = diagnostic.dfm_offset {
            if diagnostic.location.map(|l| l.file) != Some(buffer_file) {
                message = format!("{message} (in the form; dfm offset {offset})");
            }
        }
    }

    Diagnostic {
        range,
        severity: Some(severity_of(diagnostic)),
        source: Some(source_label(diagnostic.source).to_string()),
        message,
        ..Diagnostic::default()
    }
}

/// A zero-length range at the very top of the document — the best-effort
/// unit-level anchor for a finding that has no honest range in this buffer.
fn top_of_document() -> Range {
    Range {
        start: Position { line: 0, character: 0 },
        end: Position { line: 0, character: 0 },
    }
}

/// Honest severity. The parser's findings are warnings by nature (an unknown
/// `{$IF}`, a recovered declaration, a dangling dfm component) — they do not
/// necessarily mean the code fails to compile, so they map to WARNING, not
/// ERROR. Sharper per-finding severities are a later refinement.
fn severity_of(_diagnostic: &UnifiedDiagnostic) -> DiagnosticSeverity {
    DiagnosticSeverity::WARNING
}

fn source_label(source: DiagnosticSource) -> &'static str {
    match source {
        DiagnosticSource::Parse => "delphi",
        DiagnosticSource::Dfm => "delphi-dfm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::meta::{CodeLocation, Span};

    fn parse_diag(file: FileId, start: usize, end: usize, message: &str) -> UnifiedDiagnostic {
        UnifiedDiagnostic {
            source: DiagnosticSource::Parse,
            location: Some(CodeLocation {
                file,
                span: Span::new(start, end),
            }),
            dfm_offset: None,
            message: message.to_string(),
        }
    }

    #[test]
    fn parse_diagnostic_in_buffer_maps_to_exact_range() {
        // "unit X;\n{$IF Foo}\n" — a diagnostic on the {$IF} directive on line 1.
        let text = "unit X;\n{$IF Foo}\n";
        let index = LineIndex::new(text);
        let buffer = FileId(7);
        // span of "{$IF Foo}" : starts at byte 8 (after "unit X;\n"), len 9
        let start = 8;
        let end = 17;
        let diagnostics = to_lsp_diagnostics(
            &[parse_diag(buffer, start, end, "unknown {$IF}")],
            buffer,
            &index,
        );
        assert_eq!(diagnostics.len(), 1);
        let range = diagnostics[0].range;
        assert_eq!(range.start, Position { line: 1, character: 0 });
        assert_eq!(range.end, Position { line: 1, character: 9 });
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn diagnostic_in_other_file_is_not_given_a_fabricated_range() {
        // A diagnostic whose location is in a DIFFERENT file than the buffer
        // must anchor at the top of the document, never fabricate a pas range.
        let index = LineIndex::new("unit X;\n");
        let buffer = FileId(1);
        let other_file = FileId(2);
        let diagnostics =
            to_lsp_diagnostics(&[parse_diag(other_file, 100, 200, "elsewhere")], buffer, &index);
        assert_eq!(diagnostics[0].range, top_of_document());
    }

    #[test]
    fn dfm_only_offset_is_not_mapped_to_a_pas_range() {
        // A DFM finding with only a dfm offset (no pas location) must anchor at
        // the top of the document with the offset noted — never a pas range.
        let index = LineIndex::new("unit Form1;\n");
        let buffer = FileId(3);
        let dfm = UnifiedDiagnostic {
            source: DiagnosticSource::Dfm,
            location: None,
            dfm_offset: Some(42),
            message: "dangling component Ghost".to_string(),
        };
        let diagnostics = to_lsp_diagnostics(&[dfm], buffer, &index);
        assert_eq!(diagnostics[0].range, top_of_document());
        assert!(diagnostics[0].message.contains("dfm offset 42"));
        assert_eq!(diagnostics[0].source.as_deref(), Some("delphi-dfm"));
    }
}
