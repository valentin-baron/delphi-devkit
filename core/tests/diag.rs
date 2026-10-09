use ddk_core::projects::CompilerLineDiagnostic;

#[test]
fn parses_full_format_with_column() {
    let line = r"C:\Projects\Unit1.pas(42,5): error E2003: Undeclared identifier: 'Foo' [C:\Projects\MyProject.dproj]";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into());
    assert!(diag.is_some());
    let diag = diag.unwrap();
    assert_eq!(diag.file, r"C:\Projects\Unit1.pas");
    assert_eq!(diag.line, 42);
    assert_eq!(diag.column, Some(5));
    assert_eq!(diag.code, "E2003");
    assert_eq!(diag.message, "Undeclared identifier: 'Foo'");
    assert_eq!(diag.compiler_name, "dcc32");
}

#[test]
fn parses_format_without_column() {
    let line = r"Unit1.pas(10): warning W1000: Symbol 'X' is deprecated";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc64".into());
    assert!(diag.is_some());
    let diag = diag.unwrap();
    assert_eq!(diag.file, "Unit1.pas");
    assert_eq!(diag.line, 10);
    assert_eq!(diag.column, None);
    assert_eq!(diag.code, "W1000");
}

#[test]
fn parses_hint() {
    let line = r"Unit2.pas(100): hint H2164: Variable 'Y' is declared but never used";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.code, "H2164");
    assert!(format!("{}", diag.kind) == "HINT");
}

#[test]
fn parses_fatal_error() {
    let line = r"Unit3.pas(1): fatal F2039: Could not create output file";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.code, "F2039");
    // Fatal errors start with 'F', which falls through to ERROR
    assert!(format!("{}", diag.kind) == "ERROR");
}

#[test]
fn parses_delphi2007_msbuild_wrapper_format() {
    let line = r"C:\WINDOWS\Microsoft.NET\Framework\v2.0.50727\Borland.Delphi.Targets : warning : C:\Projects\Sample\SampleMessage.pas(107) Warnung: W1036 Variable 'aHelpContext' ist moeglicherweise nicht initialisiert worden [c:\Projects\Sample\Sample.dproj]";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into());
    assert!(diag.is_some());
    let diag = diag.unwrap();
    assert_eq!(diag.file, r"C:\Projects\Sample\SampleMessage.pas");
    assert_eq!(diag.line, 107);
    assert_eq!(diag.column, None);
    assert_eq!(diag.code, "W1036");
    assert_eq!(format!("{}", diag.kind), "WARN");
}

#[test]
fn parses_delphi2007_simple_indented_format() {
    let line = "  C:\\Projects\\Sample\\SampleMessage.pas(107) Warnung: W1036 Variable 'aHelpContext' ist moeglicherweise nicht initialisiert worden";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into());
    assert!(diag.is_some());
    let diag = diag.unwrap();
    assert_eq!(diag.file, r"C:\Projects\Sample\SampleMessage.pas");
    assert_eq!(diag.line, 107);
    assert_eq!(diag.code, "W1036");
    assert_eq!(format!("{}", diag.kind), "WARN");
}

// Native dcc output (Delphi 12): the severity label is localized and glued to
// the closing parenthesis, and the message code carries no trailing colon.

#[test]
fn parses_delphi12_german_warning_with_glued_label() {
    let line = "  C:\\Projects\\Sample\\Source\\Framework\\SampleLibrary.pas(205)Warnung: W1057 Implizite String-Umwandlung von 'AnsiString' zu 'WideString'";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(
        diag.file,
        "C:\\Projects\\Sample\\Source\\Framework\\SampleLibrary.pas"
    );
    assert_eq!(diag.line, 205);
    assert_eq!(diag.column, None);
    assert_eq!(diag.code, "W1057");
    assert_eq!(format!("{}", diag.kind), "WARN");
    assert_eq!(
        diag.message,
        "Implizite String-Umwandlung von 'AnsiString' zu 'WideString'"
    );
}

#[test]
fn parses_delphi12_german_hint_with_glued_label() {
    let line = "  C:\\Projects\\Sample\\Source\\Components\\SampleHelper.pas(47)Hinweis: H2219 Das private-Symbol 'fSampleProvider' wurde deklariert, aber nie verwendet";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(
        diag.file,
        "C:\\Projects\\Sample\\Source\\Components\\SampleHelper.pas"
    );
    assert_eq!(diag.line, 47);
    assert_eq!(diag.code, "H2219");
    assert_eq!(format!("{}", diag.kind), "HINT");
    assert_eq!(
        diag.message,
        "Das private-Symbol 'fSampleProvider' wurde deklariert, aber nie verwendet"
    );
}

#[test]
fn parses_delphi12_english_warning_with_glued_label() {
    let line = "  C:\\Projects\\Unit1.pas(205)Warning: W1057 Implicit string cast from 'AnsiString' to 'WideString'";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\Projects\Unit1.pas");
    assert_eq!(diag.line, 205);
    assert_eq!(diag.code, "W1057");
    assert_eq!(format!("{}", diag.kind), "WARN");
}

#[test]
fn parses_native_format_with_column_and_multiword_label() {
    let line = "  C:\\Projects\\Unit1.pas(12,7)Schwerwiegender Fehler: F2063 Erforderliche Datei nicht gefunden";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\Projects\Unit1.pas");
    assert_eq!(diag.line, 12);
    assert_eq!(diag.column, Some(7));
    assert_eq!(diag.code, "F2063");
    assert_eq!(format!("{}", diag.kind), "ERROR");
}

// The three path spellings below are verbatim output of dcc32 18.5 (Delphi
// 2007), 35.0, 36.0 (Delphi 12) and 37.0: the compiler prints the unit exactly
// as it resolved it – absolute only when the search path was – and does not
// indent its own output.

#[test]
fn parses_native_format_with_relative_path_and_no_indent() {
    let line = "src\\Unit1.pas(9) Hinweis: H2164 Variable 'x' wurde deklariert, aber nie verwendet";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"src\Unit1.pas");
    assert_eq!(diag.line, 9);
    assert_eq!(diag.code, "H2164");
    assert_eq!(format!("{}", diag.kind), "HINT");
}

#[test]
fn parses_native_format_with_bare_file_name() {
    let line = "Unit2.pas(24) Warnung: W1057 Implizite String-Umwandlung von 'AnsiString' zu 'string'";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, "Unit2.pas");
    assert_eq!(diag.line, 24);
    assert_eq!(diag.code, "W1057");
}

#[test]
fn parses_native_format_with_single_word_fatal_label() {
    let line = "Project4.dpr(4) Schwerwiegend: F2063 Verwendete Unit 'Unit4.pas' kann nicht compiliert werden";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, "Project4.dpr");
    assert_eq!(diag.code, "F2063");
    assert_eq!(format!("{}", diag.kind), "ERROR");
}

#[test]
fn parses_path_containing_parentheses() {
    let line = "  C:\\Program Files (x86)\\Proj\\Unit1.pas(205)Warnung: W1057 Implizite String-Umwandlung";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\Program Files (x86)\Proj\Unit1.pas");
    assert_eq!(diag.line, 205);
    assert_eq!(diag.code, "W1057");
}

#[test]
fn parses_path_whose_directory_is_a_parenthesised_number() {
    // The ambiguous case: "(2)" looks exactly like the line-number group, so the
    // file must be the longest path that still leaves a parsable tail.
    let line = "  C:\\Builds (2)\\U.pas(205)Warnung: W1057 Implizite String-Umwandlung";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\Builds (2)\U.pas");
    assert_eq!(diag.line, 205);
    assert_eq!(diag.code, "W1057");
}

#[test]
fn strips_the_msbuild_node_prefix_from_the_file() {
    // MSBuild's /m console logger prefixes every line with its node id.
    let native = "3>  C:\\P\\U.pas(205)Warnung: W1057 Implizite String-Umwandlung";
    let diag = CompilerLineDiagnostic::from_line(native, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\P\U.pas");

    let msbuild = r"12>C:\P\U.pas(205,3): warning W1057: Implizite String-Umwandlung [C:\P\My.dproj]";
    let diag = CompilerLineDiagnostic::from_line(msbuild, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\P\U.pas");
    assert_eq!(diag.column, Some(3));
}

// Messages ending in brackets: only MSBuild appends "[<project>]".

#[test]
fn native_message_keeps_its_own_trailing_bracket_group() {
    // Verbatim dcc32 output: native lines never carry a project suffix, so
    // nothing may be stripped off the end of the message.
    let line = "Unit5.pas(4) Warnung: W1054 Variable ist vom Typ array [0..9]";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.message, "Variable ist vom Typ array [0..9]");
}

#[test]
fn delphi2007_wrapper_message_keeps_brackets_but_loses_the_project_suffix() {
    let line = r"C:\WINDOWS\Microsoft.NET\Framework\v2.0.50727\Borland.Delphi.Targets : warning : C:\Projects\Sample\Unit5.pas(4) Warnung: W1054 Variable ist vom Typ array [0..9] [c:\Projects\Sample\Sample.dproj]";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.file, r"C:\Projects\Sample\Unit5.pas");
    assert_eq!(diag.message, "Variable ist vom Typ array [0..9]");
}

#[test]
fn msbuild_message_keeps_brackets_but_loses_the_project_suffix() {
    // Verbatim MSBuild output for the same unit.
    let line = r"Unit5.pas(4): warning W1054: Variable ist vom Typ array [0..9] [C:\P\Project5.dproj]";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    assert_eq!(diag.message, "Variable ist vom Typ array [0..9]");
}

#[test]
fn delphi2007_duplicate_output_shares_the_dedup_key() {
    // Delphi 2007 prints each diagnostic twice with identical text – once through
    // the Borland.Delphi.Targets wrapper, once plain. Only that pair may collapse.
    let wrapper = r"C:\WINDOWS\Microsoft.NET\Framework\v2.0.50727\Borland.Delphi.Targets : warning : C:\Projects\Sample\SampleMessage.pas(107) Warnung: W1036 Variable 'aHelpContext' ist moeglicherweise nicht initialisiert worden [c:\Projects\Sample\Sample.dproj]";
    let plain = "  C:\\Projects\\Sample\\SampleMessage.pas(107) Warnung: W1036 Variable 'aHelpContext' ist moeglicherweise nicht initialisiert worden";
    let a = CompilerLineDiagnostic::from_line(wrapper, "dcc32".into()).unwrap();
    let b = CompilerLineDiagnostic::from_line(plain, "dcc32".into()).unwrap();
    assert_eq!(a.dedup_key(), b.dedup_key());
}

#[test]
fn same_code_on_one_line_with_different_messages_keeps_both() {
    // Verbatim dcc32 output for a call whose two arguments are both converted
    // implicitly: same file, line and code, different message.
    let first = "Unit3.pas(25) Warnung: W1057 Implizite String-Umwandlung von 'AnsiString' zu 'WideString'";
    let second = "Unit3.pas(25) Warnung: W1057 Implizite String-Umwandlung von 'ShortString' zu 'WideString'";
    let a = CompilerLineDiagnostic::from_line(first, "dcc32".into()).unwrap();
    let b = CompilerLineDiagnostic::from_line(second, "dcc32".into()).unwrap();
    assert_eq!((&a.file, a.line, &a.code), (&b.file, b.line, &b.code));
    assert_ne!(a.dedup_key(), b.dedup_key());
}

#[test]
fn dedup_key_ignores_the_case_of_the_drive_letter() {
    let upper = "C:\\P\\U.pas(205) Warnung: W1057 Implizite String-Umwandlung";
    let lower = "c:\\P\\U.pas(205) Warnung: W1057 Implizite String-Umwandlung";
    let a = CompilerLineDiagnostic::from_line(upper, "dcc32".into()).unwrap();
    let b = CompilerLineDiagnostic::from_line(lower, "dcc32".into()).unwrap();
    assert_ne!(a.file, b.file);
    assert_eq!(a.dedup_key(), b.dedup_key());
}

// False-alarm guards: MSBuild's own messages are not Delphi diagnostics.
// Anything may precede the "(<line>)" group, so the file capture must stop at
// characters a Windows path cannot contain.

#[test]
fn rejects_quoted_command_echo_with_parenthesised_number() {
    let line = r#"  cmd /c "copy a.txt b.txt" (3)Warnung: W0001 irgendwas"#;
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn wrapper_head_never_ends_up_in_the_file_capture() {
    // Borland.Delphi.Targets hands MSBuild the severity word in English, so a
    // German "Warnung" there is no form the wrapper regex reads. The native
    // regex must then reject the line rather than capture the wrapper head as
    // part of the path: a lost diagnostic is recoverable, one published against
    // a file that does not exist is not.
    let line = r"C:\WINDOWS\x\Borland.Delphi.Targets : Warnung : C:\Projects\X.pas(107) Warnung: W1036 Variable nicht initialisiert [c:\Projects\Sample.dproj]";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_native_shape_without_a_colon_after_the_label() {
    let line = "  C:\\P\\U.pas(205)Warnung W1057 Implizite String-Umwandlung";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_line_whose_file_capture_would_be_empty() {
    let line = "(205) Warnung: W1057 Implizite String-Umwandlung";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_msbuild_own_warning_with_msb_code() {
    let line = r#"Sample\D12\Sample.dproj(202,5): warning MSB4011: "C:\Program Files (x86)\Embarcadero\Studio\23.0\bin\CodeGear.Delphi.Targets" kann nicht erneut importiert werden."#;
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_msbuild_own_error_with_msb_code() {
    let line = r"MSBUILD : error MSB1009: Projektdatei nicht vorhanden.";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_path_only_progress_line() {
    let line = "  C:\\Projects\\Sample\\Source\\Framework\\SampleLibrary";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_path_with_line_number_but_no_message() {
    let line = "  C:\\Projects\\Sample\\Unit1.pas(205)";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_label_without_message_code() {
    let line = "  C:\\Projects\\Sample\\Unit1.pas(205)Warnung: etwas ist passiert";
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_msbuild_progress_line() {
    let line = r#"Der Buildvorgang fuer das Projekt "C:\Projects\Sample\D12\Sample.dproj" wurde beendet (Clean;Build Ziele)."#;
    assert!(CompilerLineDiagnostic::from_line(line, "dcc32".into()).is_none());
}

#[test]
fn rejects_non_matching_line() {
    assert!(CompilerLineDiagnostic::from_line("Build succeeded.", "dcc32".into()).is_none());
}

#[test]
fn rejects_empty_string() {
    assert!(CompilerLineDiagnostic::from_line("", "dcc32".into()).is_none());
}

#[test]
fn rejects_random_text() {
    assert!(CompilerLineDiagnostic::from_line("Something completely different", "dcc32".into()).is_none());
}

#[test]
fn severity_error_from_e_prefix() {
    let line = r"file.pas(1): error E1234: some error";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc".into()).unwrap();
    assert_eq!(format!("{}", diag.kind), "ERROR");
}

#[test]
fn severity_warning_from_w_prefix() {
    let line = r"file.pas(1): warning W5678: some warning";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc".into()).unwrap();
    assert_eq!(format!("{}", diag.kind), "WARN");
}

#[test]
fn severity_hint_from_h_prefix() {
    let line = r"file.pas(1): hint H9999: some hint";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc".into()).unwrap();
    assert_eq!(format!("{}", diag.kind), "HINT");
}

#[test]
fn display_with_column() {
    let line = r"file.pas(10,5): error E2003: something";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    let display = format!("{}", diag);
    assert!(display.contains("[ERROR]"));
    assert!(display.contains("[E2003]"));
    assert!(display.contains("file.pas:10:5"));
    assert!(display.contains("something"));
}

#[test]
fn display_without_column() {
    let line = r"file.pas(10): warning W1000: something";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    let display = format!("{}", diag);
    assert!(display.contains("[WARN]"));
    assert!(display.contains("file.pas:10"));
    assert!(!display.contains("file.pas:10:"));
}

#[test]
fn lsp_diagnostic_line_is_zero_based() {
    use tower_lsp::lsp_types::Diagnostic;

    let line = r"file.pas(10,5): error E2003: msg";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    let lsp_diag: Diagnostic = diag.into();
    // MSBuild line 10 → LSP line 9
    assert_eq!(lsp_diag.range.start.line, 9);
    // MSBuild column 5 → LSP character 4
    assert_eq!(lsp_diag.range.start.character, 4);
}

#[test]
fn lsp_diagnostic_severity_mapping() {
    use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

    let err_line = r"file.pas(1): error E1000: err";
    let err_diag: Diagnostic = CompilerLineDiagnostic::from_line(err_line, "dcc".into()).unwrap().into();
    assert_eq!(err_diag.severity, Some(DiagnosticSeverity::ERROR));

    let warn_line = r"file.pas(1): warning W1000: warn";
    let warn_diag: Diagnostic = CompilerLineDiagnostic::from_line(warn_line, "dcc".into()).unwrap().into();
    assert_eq!(warn_diag.severity, Some(DiagnosticSeverity::WARNING));

    let hint_line = r"file.pas(1): hint H1000: hint";
    let hint_diag: Diagnostic = CompilerLineDiagnostic::from_line(hint_line, "dcc".into()).unwrap().into();
    assert_eq!(hint_diag.severity, Some(DiagnosticSeverity::HINT));
}

#[test]
fn lsp_diagnostic_no_column_defaults_to_zero() {
    use tower_lsp::lsp_types::Diagnostic;

    let line = r"file.pas(5): error E2003: msg";
    let diag = CompilerLineDiagnostic::from_line(line, "dcc32".into()).unwrap();
    let lsp_diag: Diagnostic = diag.into();
    // No column → default 1, minus 1 → 0
    assert_eq!(lsp_diag.range.start.character, 0);
}

#[test]
fn lsp_diagnostic_source_is_compiler_name() {
    use tower_lsp::lsp_types::Diagnostic;

    let line = r"file.pas(1): error E1000: msg";
    let diag = CompilerLineDiagnostic::from_line(line, "my-compiler".into()).unwrap();
    let lsp_diag: Diagnostic = diag.into();
    assert_eq!(lsp_diag.source, Some("my-compiler".to_string()));
}
