use chrono::{DateTime, Local};
use tower_lsp::lsp_types::*;
use std::fmt::Display;

const MSBUILD_OUTPUT_REGEX: &str = r"^(?P<file>.*?)[(](?P<line>\d+)(?:,(?P<column>\d+))?[)]:\s+(?P<kind>.*?)\s+(?P<code>[A-Z]\d+):\s+(?P<message>.*?)(?:\s+\[.*\])?$";

#[derive(Debug)]
pub enum DiagnosticKind {
    ERROR,
    WARN,
    HINT,
}

impl Display for DiagnosticKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagnosticKind::ERROR => write!(f, "ERROR"),
            DiagnosticKind::WARN => write!(f, "WARN"),
            DiagnosticKind::HINT => write!(f, "HINT"),
        }
    }
}

pub struct CompilerLineDiagnostic {
    pub time: DateTime<Local>,
    pub file: String,
    pub line: u32,
    pub column: Option<u32>,
    pub message: String,
    pub code: String,
    pub kind: DiagnosticKind,
    pub compiler_name: String,
}

impl Display for CompilerLineDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let time = self.time.format("%H:%M:%S.%.3f");
        let kind = &self.kind;
        let code = &self.code;
        let file = &self.file;
        let line = &self.line;
        let message = &self.message;
        if let Some(column) = self.column {
            write!(
                f,
                "{time}: [{kind}][{code}] {file}:{line}:{column} - {message}",
            )
        } else {
            write!(
                f,
                "{time}: [{kind}][{code}] {file}:{line} - {message}"
            )
        }
    }
}

impl CompilerLineDiagnostic {
    pub fn from_line(line: &str, compiler_name: String) -> Option<Self> {
        if let Some(captures) = regex::Regex::new(MSBUILD_OUTPUT_REGEX).unwrap().captures(line) {
            let file = captures.name("file")?.as_str().to_string();
            let line = captures.name("line")?.as_str().parse().ok()?;
            let column = captures
                .name("column")
                .and_then(|m| m.as_str().parse().ok());
            let message = captures.name("message")?.as_str().to_string();
            let code = captures.name("code")?.as_str().to_string();
            let kind = if code.starts_with('H') {
                DiagnosticKind::HINT
            } else if code.starts_with('W') {
                DiagnosticKind::WARN
            } else {
                DiagnosticKind::ERROR
            };

            Some(CompilerLineDiagnostic {
                time: Local::now(),
                file,
                line,
                column,
                message,
                code,
                kind,
                compiler_name
            })
        } else {
            None
        }
    }
}

impl Into<Diagnostic> for CompilerLineDiagnostic {
    fn into(self) -> Diagnostic {
        return Diagnostic {
            range: Range {
                start: Position {
                    line: self.line.saturating_sub(1),
                    character: self.column.unwrap_or(1).saturating_sub(1),
                },
                end: Position {
                    line: self.line.saturating_sub(1),
                    character: self.column.unwrap_or(1).saturating_sub(1) + 1,
                },
            },
            severity: match self.kind {
                DiagnosticKind::ERROR => Some(DiagnosticSeverity::ERROR),
                DiagnosticKind::WARN => Some(DiagnosticSeverity::WARNING),
                DiagnosticKind::HINT => Some(DiagnosticSeverity::HINT),
            },
            code: Some(NumberOrString::String(self.code.clone())),
            source: Some(self.compiler_name.to_string()),
            message: self.message.clone(),
            ..Default::default()
        };
    }
}
