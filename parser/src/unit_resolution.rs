//! Unit name → source file resolution, mirroring the compiler's order:
//!
//! 1. `DCC_UnitAlias` substitution on the full name (`WinTypes` → `Windows`).
//! 2. The name as given.
//! 3. For undotted names: each `DCC_Namespace` prefix (`SysUtils` →
//!    `System.SysUtils`), in declared order — first hit wins.
//!
//! Every candidate is probed as `<candidate>.pas` across the search paths in
//! order (project search paths first; the driver appends the compiler's
//! standard-source directories at session open). Windows filesystems are
//! case-insensitive — no case variants are probed.
//!
//! Not handled here (explicitly): `in 'path'` clauses (project-file scoped,
//! the caller resolves those directly) and DCU-only units (no source to
//! find — the loader reports NotFound; sidecar manifests are the plan).

use std::path::PathBuf;

use crate::context::ProjectContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUnit {
    pub path: PathBuf,
    /// The name that actually matched — `System.SysUtils` when `SysUtils`
    /// resolved through a namespace prefix. Cache identity uses this.
    pub effective_name: String,
}

pub fn resolve_unit(context: &ProjectContext, unit_name: &str) -> Option<ResolvedUnit> {
    // alias substitution operates on the whole name; fold through the ONE
    // identifier fold so the lookup key matches how aliases were interned
    // (`intern_key` = `fold_identifier`), consistent for non-ASCII too.
    let aliased = crate::globals::interner()
        .get(crate::globals::fold_identifier(unit_name))
        .map(crate::context::Identifier::from)
        .and_then(|key| context.unit_aliases.get(&key))
        .map(|target| crate::globals::resolve(*target).to_string());
    let base_name = aliased.unwrap_or_else(|| unit_name.to_string());

    let mut candidates = vec![base_name.clone()];
    if !base_name.contains('.') {
        for namespace in &context.namespaces {
            candidates.push(format!("{namespace}.{base_name}"));
        }
    }

    for candidate in candidates {
        for directory in &context.search_paths {
            let path = directory.join(format!("{candidate}.pas"));
            if path.is_file() {
                return Some(ResolvedUnit {
                    path,
                    effective_name: candidate,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;

    fn context_with(
        search_paths: Vec<PathBuf>,
        namespaces: Vec<String>,
        aliases: &[(&str, &str)],
    ) -> ProjectContext {
        let context = ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths,
            include_paths: Vec::new(),
            namespaces,
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        };
        let mut context = context;
        for (old, actual) in aliases {
            let old = context.intern_key(old);
            let actual = context.intern_key(actual);
            context.unit_aliases.insert(old, actual);
        }
        context
    }

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir()
            .join("delphi_parser_resolution")
            .join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn direct_name_and_search_path_order() {
        let first = temp_directory("order_first");
        let second = temp_directory("order_second");
        std::fs::write(second.join("Thing.pas"), "unit Thing;").unwrap();
        std::fs::write(first.join("Thing.pas"), "unit Thing;").unwrap();

        let context = context_with(vec![first.clone(), second], Vec::new(), &[]);
        let resolved = resolve_unit(&context, "Thing").unwrap();
        assert_eq!(resolved.path, first.join("Thing.pas"));
        assert_eq!(resolved.effective_name, "Thing");
    }

    #[test]
    fn namespace_prefix_resolution() {
        let directory = temp_directory("namespaces");
        std::fs::write(directory.join("System.SysUtils.pas"), "unit System.SysUtils;").unwrap();

        let context = context_with(
            vec![directory.clone()],
            vec!["Winapi".to_string(), "System".to_string()],
            &[],
        );
        let resolved = resolve_unit(&context, "SysUtils").unwrap();
        assert_eq!(resolved.effective_name, "System.SysUtils");
        // dotted names do NOT get prefixes
        assert!(resolve_unit(&context, "Foo.SysUtils").is_none());
    }

    #[test]
    fn direct_match_beats_namespace_prefix() {
        let directory = temp_directory("direct_wins");
        std::fs::write(directory.join("SysUtils.pas"), "unit SysUtils;").unwrap();
        std::fs::write(directory.join("System.SysUtils.pas"), "unit System.SysUtils;").unwrap();

        let context = context_with(vec![directory.clone()], vec!["System".to_string()], &[]);
        assert_eq!(
            resolve_unit(&context, "SysUtils").unwrap().effective_name,
            "SysUtils"
        );
    }

    #[test]
    fn alias_substitution() {
        let directory = temp_directory("aliases");
        std::fs::write(directory.join("WINDOWS.pas"), "unit Windows;").unwrap();

        let context = context_with(vec![directory], Vec::new(), &[("WinTypes", "Windows")]);
        let resolved = resolve_unit(&context, "wintypes").unwrap();
        assert!(resolved.path.file_name().unwrap().eq_ignore_ascii_case("WINDOWS.pas"));
    }

    #[test]
    fn missing_unit_is_none() {
        let context = context_with(Vec::new(), Vec::new(), &[]);
        assert!(resolve_unit(&context, "Nowhere").is_none());
    }
}
