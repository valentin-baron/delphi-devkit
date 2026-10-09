//! End-to-end check of range mapping against the *real* Delphi formatter — the
//! same `Formatter` path that `ddk format` uses. Skipped when the machine has
//! no `Formatter.exe`.

use ddk_core::format::range::map_range;
use ddk_core::format::Formatter;
use ddk_core::projects::CompilerConfigurations;
use ddk_core::state::Stateful;

const MESSY: &str = "unit Messy;\n\
interface\n\
implementation\n\
procedure Foo;\n\
begin\n\
x:=1;   y:=2;\n\
if x=1 then   z:=3;\n\
end;\n\
end.\n";

/// Content is ASCII, so the edit's UTF-16 offsets coincide with byte offsets
/// and can index the &str directly.
fn apply(original: &str, start: usize, end: usize, new_text: &str) -> String {
    format!("{}{}{}", &original[..start], new_text, &original[end..])
}

#[tokio::test]
async fn range_format_matches_the_real_formatter() {
    CompilerConfigurations::initialize().expect("initialize compilers");
    if CompilerConfigurations::first_available_formatter().await.is_none() {
        eprintln!("skipping: no Delphi Formatter.exe found on this machine");
        return;
    }

    // Ground truth: format the whole file exactly as `ddk format` does.
    let formatted = Formatter::new(MESSY.to_string())
        .expect("build formatter")
        .execute()
        .await
        .expect("run formatter");

    // 1) A statement at column 0 the formatter indents; being the first token
    //    on its line, the new indentation is part of the edit.
    let sel = "x:=1;";
    let start = MESSY.find(sel).unwrap();
    let edit = map_range(MESSY, &formatted, start, start + sel.len());
    assert_eq!(edit.new_text, "  x := 1;");
    let applied = apply(MESSY, edit.start, edit.end, &edit.new_text);
    assert!(applied.contains("  x := 1;"));
    assert!(applied.contains("y:=2;"), "text outside the selection is untouched");

    // 2) A selection the formatter splits across lines. It also normalizes line
    //    endings to CRLF — a whitespace-only change the mapping is immune to.
    let sel = "if x=1 then   z:=3;";
    let start = MESSY.find(sel).unwrap();
    let edit = map_range(MESSY, &formatted, start, start + sel.len());
    assert_eq!(edit.new_text, "  if x = 1 then\r\n    z := 3;");

    // 3) Applying it must equal the formatter's own take on that region.
    let applied = apply(MESSY, edit.start, edit.end, &edit.new_text);
    assert!(applied.contains("  if x = 1 then\r\n    z := 3;"));
}
