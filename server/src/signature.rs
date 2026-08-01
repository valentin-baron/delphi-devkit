//! Build an LSP [`SignatureHelp`] from the parser's `signature_help` query,
//! honestly.
//!
//! The composition: a callee byte offset + active parameter index (from
//! [`crate::call_context`]) → resolve the identifier there via `symbol_at` →
//! the parser `signature_help` query (reads params/return from the AST,
//! cross-unit via the SAME loader as definition) → LSP `SignatureInformation`s.
//!
//! NEVER a fabricated signature: if nothing resolves at the callee offset, or
//! the callee does not resolve to a routine, this returns `None` (the editor
//! shows nothing) — never a made-up `(...)`.

use tower_lsp::lsp_types::{
    Documentation, ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};

use delphi_parser::driver::ProjectSession;
use delphi_parser::query::SignatureInfo;

/// Resolve signature help for a callee at `callee_offset` with `active_parameter`
/// in `unit_key`'s source. Factored out of the server handler so the composition
/// (symbol → signature_help → LSP mapping) is unit-testable without a live
/// `Client`.
///
/// `None` (never a fabricated signature) when: no identifier resolves at the
/// callee offset, or the callee resolves to something that is not a routine
/// (the parser query returns an empty set). `active_parameter` is clamped to the
/// last parameter so a trailing/extra comma highlights the final parameter
/// rather than pointing past the end.
pub fn resolve_signature_help(
    session: &ProjectSession,
    unit_key: delphi_parser::context::Identifier,
    callee_offset: u32,
    active_parameter: u32,
) -> Option<SignatureHelp> {
    // The parser resolves the callee identifier at this offset AND its owner (a
    // member call's receiver type), then reads params/return from the AST,
    // cross-unit via the SAME loader as definition. Empty ⇒ the callee is not a
    // routine (or unresolved) ⇒ None (never a fabricated signature).
    let signatures = session.signature_help_at(unit_key, callee_offset);
    if signatures.is_empty() {
        return None;
    }

    let signature_informations: Vec<SignatureInformation> = signatures
        .iter()
        .map(|signature| to_signature_information(signature, active_parameter))
        .collect();

    Some(SignatureHelp {
        signatures: signature_informations,
        // The first signature is active. Distinguishing the "best" overload for
        // the current arguments would require type-checking the arguments, which
        // the query layer does not do — so we present all overloads and default
        // to the first (the editor lets the user cycle). Documented limitation.
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

/// Map one parser [`SignatureInfo`] to an LSP [`SignatureInformation`], with a
/// per-parameter label and the active-parameter index clamped to this
/// signature's parameter count.
fn to_signature_information(
    signature: &SignatureInfo,
    active_parameter: u32,
) -> SignatureInformation {
    let parameters: Vec<ParameterInformation> = signature
        .parameters
        .iter()
        .map(|parameter| ParameterInformation {
            // A string label (not offsets): each parameter label is a
            // self-contained substring the editor matches within the signature
            // label. Robust against the grouped-parameter expansion.
            label: ParameterLabel::Simple(parameter.label.clone()),
            documentation: None::<Documentation>,
        })
        .collect();

    // Clamp the active parameter to the last real parameter for THIS signature
    // (an overload with fewer parameters, or a trailing comma, must not point
    // past the end — never a wrong highlight).
    let clamped = if parameters.is_empty() {
        None
    } else {
        Some(active_parameter.min(parameters.len() as u32 - 1))
    };

    SignatureInformation {
        label: signature.label.clone(),
        documentation: None,
        parameters: Some(parameters),
        active_parameter: clamped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::cache_store::{CacheIdentity, CacheStore};
    use delphi_parser::context::{
        DefineSet, ProjectContext, SwitchState, TargetPlatform,
    };
    use delphi_parser::unit_cache::UnitCache;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    fn session_in(directory: &Path) -> ProjectSession {
        let context = ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: vec![directory.to_path_buf()],
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        };
        let project = directory.join("proj.dproj");
        std::fs::write(&project, b"<Project/>").unwrap();
        let identity = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        let store = CacheStore::in_directory(directory, &identity).unwrap();
        ProjectSession::from_parts(Arc::new(context), store, Duration::from_secs(300))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("ddk-server-signature").join(tag);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// A top-level routine, called from the implementation body: the callee
    /// offset (the `Add` usage) resolves to the routine and its signature is
    /// built, with the active parameter clamped.
    #[test]
    fn top_level_routine_signature_with_active_parameter() {
        let directory = temp_dir("top_routine");
        std::fs::write(
            directory.join("Calc.pas"),
            "unit Calc;\ninterface\n\
             function Add(X: Integer; Y: Integer): Integer;\n\
             implementation\n\
             procedure Use;\nbegin\n  Add(1, 2);\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("Calc.pas")).unwrap();
        let key = session.context().intern_key("CALC");
        let content = std::fs::read_to_string(directory.join("Calc.pas")).unwrap();
        // the CALL-SITE `Add` in the body (not the declaration).
        let callee = content.rfind("Add(").unwrap() as u32;

        let help = resolve_signature_help(&session, key, callee, 1).expect("signature");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "function Add(X: Integer; Y: Integer): Integer");
        assert_eq!(help.active_parameter, Some(1));
        // parameter labels are the rendered substrings
        let parameters = help.signatures[0].parameters.as_ref().unwrap();
        assert_eq!(parameters.len(), 2);
        assert!(matches!(
            &parameters[0].label,
            ParameterLabel::Simple(label) if label == "X: Integer"
        ));
    }

    /// A cross-unit STATIC method call `TUser.Compute(...)`: the callee offset
    /// lands in `Compute` (the last dotted segment); the parser resolves the
    /// receiver `TUser` (an imported type used as a static scope) and reads the
    /// imported class method's signature. (An INSTANCE receiver whose declared
    /// type the derived index does not carry is a documented limitation; a static
    /// type receiver resolves honestly.)
    #[test]
    fn cross_unit_static_method_signature() {
        let directory = temp_dir("cross_method");
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\
             type TUser = class\npublic\n\
               class function Compute(const A: Integer): Boolean;\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("App.pas"),
            "unit App;\ninterface\nuses Models;\n\
             implementation\n\
             procedure Run;\nbegin\n  TUser.Compute(5);\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("App.pas")).unwrap();
        let key = session.context().intern_key("APP");
        let content = std::fs::read_to_string(directory.join("App.pas")).unwrap();
        // offset inside `Compute` in `TUser.Compute(5)`.
        let callee = content.find("Compute(").unwrap() as u32;

        let help = resolve_signature_help(&session, key, callee, 0).expect("cross-unit signature");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(
            help.signatures[0].label,
            "function Compute(const A: Integer): Boolean"
        );
    }

    /// The callee does not resolve to a routine (an unknown name / a non-routine)
    /// → `None`, never a fabricated signature.
    #[test]
    fn unresolved_callee_is_none() {
        let directory = temp_dir("unresolved");
        std::fs::write(
            directory.join("U.pas"),
            "unit U;\ninterface\n\
             type TThing = class end;\n\
             implementation\n\
             procedure Use;\nbegin\n  TThing(0);\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let key = session.context().intern_key("U");
        let content = std::fs::read_to_string(directory.join("U.pas")).unwrap();
        // `TThing(` is a type cast, not a routine call → no signature.
        let callee = content.rfind("TThing(").unwrap() as u32;
        assert!(
            resolve_signature_help(&session, key, callee, 0).is_none(),
            "a non-routine callee (type cast) must yield no signature"
        );
    }

    /// A procedure signature carries no return type, and the active parameter is
    /// clamped to the last parameter when the cursor is past the end.
    #[test]
    fn procedure_no_return_and_active_parameter_clamped() {
        let directory = temp_dir("proc_clamp");
        std::fs::write(
            directory.join("Log.pas"),
            "unit Log;\ninterface\n\
             procedure Write(const Message: string);\n\
             implementation\n\
             procedure Use;\nbegin\n  Write('x');\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("Log.pas")).unwrap();
        let key = session.context().intern_key("LOG");
        let content = std::fs::read_to_string(directory.join("Log.pas")).unwrap();
        let callee = content.rfind("Write(").unwrap() as u32;

        // active_parameter 5 is past the single parameter → clamped to 0.
        let help = resolve_signature_help(&session, key, callee, 5).expect("signature");
        assert_eq!(help.signatures[0].label, "procedure Write(const Message: string)");
        assert_eq!(
            help.signatures[0].active_parameter,
            Some(0),
            "an out-of-range active parameter clamps to the last parameter"
        );
    }
}
