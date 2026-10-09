use chrono::{DateTime, Local};
use tower_lsp::lsp_types::*;
use std::fmt::Display;

// Standard MSBuild / dcc32 format:
// <file>(<line>[,<col>]): (error|warning|hint|fatal) <CODE>: <message> [<project>]
//
// The trailing project suffix is matched as "[<no closing bracket>]" rather than
// "[<anything>]": dcc emits messages that end in brackets themselves
// ("W1054 Variable ist vom Typ array [0..9] [C:\P\My.dproj]"), and a greedy
// "[.*]" would start at the first bracket and swallow the message's own.
const MSBUILD_OUTPUT_REGEX: &str = r"^(?P<file>.*?)[(](?P<line>\d+)(?:,(?P<column>\d+))?[)]:\s+(?P<kind>.*?)\s+(?P<code>[A-Z]\d+):\s+(?P<message>.*?)(?:\s+\[[^\]]*\])?\s*$";

// A source file as the compilers print it. The path may be absolute, relative
// or a bare file name: dcc prints the unit exactly as it resolved it, so
// "C:\Proj\Unit1.pas", "src\Unit1.pas" and "Unit1.pas" all occur (verified
// against dcc32 18.5, 35.0, 36.0 and 37.0).
//
// What the capture must not do is run backwards over text that is no path, so
// it is restricted to the characters a Windows path can hold. Excluding
// `:"<>|*?` is what keeps the `<target> : warning : ` head of the wrapper line
// below and a quoted command echo (`cmd /c "copy a b" (3)`) out of the file
// name. Parentheses stay allowed because real paths contain them
// ("C:\Program Files (x86)", "C:\Builds (2)"); the `(<line>)` group that follows
// resolves that ambiguity.
const DIAG_FILE: &str = r#"(?P<file>(?:[A-Za-z]:)?[^:"<>|*?\r\n]+?)"#;

const DIAG_POSITION: &str = r"[(](?P<line>\d+)(?:,(?P<column>\d+))?[)]";

// Tail shared by the two localized dcc formats below, starting after the closing
// parenthesis of the line/column notation:
//   [whitespace]<localized_label>: <CODE> <message>
//
// The label is the compiler's severity word in the IDE language ("Warnung:",
// "Hinweis:", "Fehler:", "Schwerwiegend:", "Warning:"). It is therefore matched
// as "letters and spaces", never by a fixed word list, and the severity is
// derived from <CODE> alone. Every dcc checked separates the label from the
// parenthesis by one space; the separator stays optional so a glued spelling
// ("...pas(205)Warnung: W1057 ...") parses as well. The colon after the label is
// mandatory and keeps this tail disjoint from the MSBuild format above, where it
// is the code – not the label – that a colon follows.
const DCC_LOCALIZED_TAIL: &str = r"\s*(?:\p{L}[\p{L} ]*)?:\s*(?P<code>[A-Z]\d+)\s+(?P<message>\S.*?)";

// MSBuild appends the project file to the diagnostics it formats itself. That is
// why it is no part of the shared tail: native dcc output never carries it, and
// stripping it there would truncate the messages that end in brackets.
const MSBUILD_PROJECT_SUFFIX: &str = r"(?:\s+\[[^\]]*\])?";

// Delphi 2007 / Borland MSBuild wrapper format:
// <target_file> : (warning|error|hint|fatal) : <source_file>(<line>)<tail> [<project>]
const DELPHI2007_MSBUILD_HEAD: &str = r"^.*?\s+:\s+(?:warning|error|hint|fatal)\s+:\s+";

// Native compiler output without MSBuild wrapper: dcc32/dcc64 called directly
// for a bare .dpr as well as the raw dcc lines MSBuild passes through. The
// indentation is optional because only the pass-through is indented.
const DCC_NATIVE_HEAD: &str = r"^\s*";

// MSBuild's multi-processor console logger prefixes every line with the id of
// the node that wrote it ("3>  C:\…"). It is removed before matching instead of
// being tolerated inside each format: '>' cannot occur in a path, so a leading
// "<digits>>" is never part of a diagnostic.
const MSBUILD_NODE_PREFIX_REGEX: &str = r"^\s*\d+>";

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
        let time = self.time.format("%H:%M:%S%.3f");
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

lazy_static::lazy_static! {
    pub static ref COMPILER_OUTPUT_REGEX: regex::Regex = regex::Regex::new(MSBUILD_OUTPUT_REGEX).unwrap();
    static ref DELPHI2007_MSBUILD_OUTPUT_REGEX: regex::Regex = regex::Regex::new(&format!(
        r"{DELPHI2007_MSBUILD_HEAD}{DIAG_FILE}{DIAG_POSITION}{DCC_LOCALIZED_TAIL}{MSBUILD_PROJECT_SUFFIX}\s*$"
    )).unwrap();
    static ref DCC_NATIVE_OUTPUT_REGEX: regex::Regex = regex::Regex::new(&format!(
        r"{DCC_NATIVE_HEAD}{DIAG_FILE}{DIAG_POSITION}{DCC_LOCALIZED_TAIL}\s*$"
    )).unwrap();
    static ref MSBUILD_NODE_PREFIX: regex::Regex = regex::Regex::new(MSBUILD_NODE_PREFIX_REGEX).unwrap();
}

fn build_from_captures(captures: regex::Captures, compiler_name: String) -> Option<CompilerLineDiagnostic> {
    let file = captures.name("file")?.as_str().trim().to_string();
    let line_num = captures.name("line")?.as_str().parse().ok()?;
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
        line: line_num,
        column,
        message,
        code,
        kind,
        compiler_name,
    })
}

impl CompilerLineDiagnostic {
    /// Try to parse a raw compiler output line into a [`CompilerLineDiagnostic`].
    ///
    /// Attempts three formats in order:
    /// 1. Standard MSBuild / dcc32 format
    /// 2. Delphi 2007 Borland.Delphi.Targets MSBuild wrapper
    /// 3. Native dcc output (Delphi 2007 duplicate line, Delphi 12 pass-through)
    ///
    /// The wrapper format is tried before the native one because its line also
    /// ends in the native shape – matching natively first would make the file
    /// capture swallow the `<target> : warning : ` prefix.
    ///
    /// The severity is always derived from the message code, never from the
    /// label, which the compiler emits in the IDE's UI language.
    pub fn from_line(line: &str, compiler_name: String) -> Option<Self> {
        let line = MSBUILD_NODE_PREFIX.replace(line, "");
        if let Some(captures) = COMPILER_OUTPUT_REGEX.captures(&line) {
            return build_from_captures(captures, compiler_name);
        }
        if let Some(captures) = DELPHI2007_MSBUILD_OUTPUT_REGEX.captures(&line) {
            return build_from_captures(captures, compiler_name);
        }
        if let Some(captures) = DCC_NATIVE_OUTPUT_REGEX.captures(&line) {
            return build_from_captures(captures, compiler_name);
        }
        None
    }

    /// Key that identifies a diagnostic for the "same as the previous one"
    /// check in the output reader.
    ///
    /// Delphi 2007 reports every diagnostic twice – once through the
    /// Borland.Delphi.Targets MSBuild wrapper and once as a plain dcc line –
    /// with character-identical text, which is why the message is part of the
    /// key: dcc legitimately reports several diagnostics with the same code for
    /// one source line (one W1057 per implicitly converted argument of a call),
    /// and those differ in nothing but the message.
    ///
    /// The file is lowercased because Windows paths are case-insensitive and the
    /// two spellings of one diagnostic need not agree on the drive letter.
    pub fn dedup_key(&self) -> (String, u32, String, String) {
        (
            self.file.to_lowercase(),
            self.line,
            self.code.clone(),
            self.message.clone(),
        )
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
