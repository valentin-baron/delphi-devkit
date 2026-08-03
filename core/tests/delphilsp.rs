//! End-to-end generation of a `.delphilsp.json` from a synthetic project and a
//! fake Delphi installation.
//!
//! The IDE's global Library Path comes from the machine's registry, so the
//! assertions here deliberately cover only what the inputs determine: the
//! project's own search-path entries, defines, namespaces, outputs, target
//! kind, and the resolved compiler DLL.

use std::path::{Path, PathBuf};

use ddk_core::delphilsp::{DccOptionsInput, GenerationRequest, build_dcc_options, generate};

/// Minimal but realistic `.dproj`: two configurations, a platform-specific
/// group, and the `;$(DCC_…)` inheritance tokens Delphi writes.
const DPROJ: &str = include_str!("fixtures/App.dproj");

/// A directory tree that looks enough like a Delphi install for the generator:
/// `bin\rsvars.bat` plus the two Windows compiler DLLs.
fn fake_installation(root: &Path) -> PathBuf {
    let installation = root.join("Studio").join("23.0");
    let bin = installation.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("rsvars.bat"),
        format!(
            "@SET BDS={}\r\n@SET BDSCOMMONDIR={}\r\n@SET PLATFORM=\r\n",
            installation.display(),
            root.join("Public").display()
        ),
    )
    .unwrap();
    std::fs::write(bin.join("dcc32290.dll"), b"").unwrap();
    std::fs::write(bin.join("dcc64290.dll"), b"").unwrap();
    std::fs::write(bin.join("dcc64290N.dll"), b"").unwrap();
    installation
}

struct Fixture {
    _temp: tempfile::TempDir,
    request: GenerationRequest,
    out_path: PathBuf,
}

fn fixture(configuration: Option<&str>, platform: Option<&str>) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let project_dir = root.join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let dproj_path = project_dir.join("App.dproj");
    std::fs::write(&dproj_path, DPROJ).unwrap();
    std::fs::write(project_dir.join("App.dpr"), "program App; begin end.").unwrap();
    let installation = fake_installation(root);
    let out_path = project_dir.join("App.delphilsp.json");
    Fixture {
        request: GenerationRequest {
            dproj_path: Some(dproj_path),
            main_source: project_dir.join("App.dpr"),
            configuration: configuration.map(|s| s.to_string()),
            platform: platform.map(|s| s.to_string()),
            installation_path: installation,
            bds_version: "23.0".to_string(),
            compiler_name: "Delphi 12.0 Athens".to_string(),
            out_path: Some(out_path.clone()),
        },
        out_path,
        _temp: temp,
    }
}

fn written_file(fixture: &Fixture) -> serde_json::Value {
    let raw = std::fs::read_to_string(&fixture.out_path).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()
}

fn written_settings(fixture: &Fixture) -> serde_json::Value {
    written_file(fixture)["settings"].clone()
}

/// Split a dcc path option (`-U…`) into its individual entries, unquoting
/// those the generator wrapped because they contain spaces.
fn path_entries(options: &str, prefix: &str) -> Vec<String> {
    let start = options.find(prefix).expect("option not found");
    let tail = &options[start + prefix.len()..];
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in tail.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' if !quoted => break,
            ';' if !quoted => entries.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries
}

#[test]
fn writes_the_full_ide_key_set() {
    let fixture = fixture(None, None);
    generate(&fixture.request).unwrap();
    let settings = written_settings(&fixture);
    for key in [
        "project",
        "dllname",
        "dccOptions",
        "projectFiles",
        "includeDCUsInUsesCompletion",
        "enableKeyWordCompletion",
        "browsingPaths",
        "CommonAppData",
        "Templates",
    ] {
        assert!(settings.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn stamps_the_ownership_marker_next_to_settings() {
    let fixture = fixture(None, None);
    generate(&fixture.request).unwrap();
    let file = written_file(&fixture);

    // The VS Code extension auto-refreshes a stale config only when it finds
    // this marker, so an IDE-written file (which has none) is never clobbered.
    assert_eq!(file["generatedBy"], ddk_core::delphilsp::GENERATED_BY_MARKER);
    assert_eq!(file["generatedBy"], "delphi-devkit");
    assert!(
        file["settings"].get("generatedBy").is_none(),
        "the marker belongs beside `settings`, not inside it"
    );
    assert_eq!(
        file.as_object().unwrap().len(),
        3,
        "expected exactly `settings`, `generatedBy` and `dprojHash`: {file}"
    );

    // The staleness fingerprint: SHA-256 of the dproj bytes, lowercase hex.
    let hash = file["dprojHash"].as_str().expect("dprojHash missing");
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hash}");
}

#[test]
fn regenerating_over_a_ddk_file_keeps_the_marker() {
    let fixture = fixture(None, None);
    generate(&fixture.request).unwrap();
    generate(&fixture.request).unwrap();
    assert_eq!(written_file(&fixture)["generatedBy"], "delphi-devkit");
}

// The drive-letter assertions (`%3A`, no literal `:`) only hold for Windows
// temp paths; on other platforms temp dirs have no drive prefix.
#[cfg(windows)]
#[test]
fn project_uri_points_at_the_main_source_not_the_dproj() {
    let fixture = fixture(None, None);
    let result = generate(&fixture.request).unwrap();
    assert!(result.project_uri.starts_with("file:///"), "{}", result.project_uri);
    assert!(result.project_uri.ends_with("/App.dpr"), "{}", result.project_uri);
    let path_part = result.project_uri.trim_start_matches("file:///");
    assert!(!path_part.contains(':'), "drive colon must be encoded: {}", result.project_uri);
    assert!(path_part.contains("%3A"), "{}", result.project_uri);
}

#[test]
fn resolves_the_platform_specific_compiler_dll() {
    let win64 = fixture(None, Some("Win64"));
    assert_eq!(generate(&win64.request).unwrap().dllname, "dcc64290.dll");
    let win32 = fixture(None, Some("Win32"));
    assert_eq!(generate(&win32.request).unwrap().dllname, "dcc32290.dll");
}

#[test]
fn debug_configuration_chains_defines_across_property_groups() {
    let fixture = fixture(Some("Debug"), Some("Win64"));
    let result = generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();
    assert!(options.contains(" -DDEBUG;APPWIDE "), "{options}");
    assert_eq!(result.define_count, 2);
}

#[test]
fn release_configuration_selects_its_own_defines_and_optimisation() {
    let fixture = fixture(Some("Release"), Some("Win64"));
    generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();
    assert!(options.contains(" -DRELEASE;APPWIDE "), "{options}");
    assert!(options.starts_with("-$O+ "), "{options}");
    assert!(!options.contains("--inline:off"), "{options}");
}

#[test]
fn namespaces_chain_platform_group_first() {
    let fixture = fixture(Some("Debug"), Some("Win64"));
    generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();
    assert!(options.contains(" -NSWinapi;System;Vcl; "), "{options}");
}

#[test]
fn project_search_paths_come_first_and_keep_their_relative_form() {
    let fixture = fixture(Some("Debug"), Some("Win64"));
    let result = generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();

    let unit_paths = path_entries(&options, " -U");
    // Debug pulls the IDE's debug-DCU directory in front when the registry
    // provides one; the project's own entries always follow immediately.
    let project_entries: Vec<&String> = unit_paths
        .iter()
        .filter(|p| p.as_str() == r".\Win64\Debug" || p.as_str() == r"..\shared")
        .collect();
    assert_eq!(project_entries.len(), 2, "{unit_paths:?}");
    let first = unit_paths.iter().position(|p| p == r".\Win64\Debug").unwrap();
    assert_eq!(unit_paths[first + 1], r"..\shared", "{unit_paths:?}");

    // -O / -R never carry the debug-DCU entry, so they start at the project's own path.
    assert_eq!(path_entries(&options, " -O")[0], r".\Win64\Debug");
    assert_eq!(path_entries(&options, " -R")[0], r".\Win64\Debug");
    assert!(result.search_path_count >= 2);
}

#[test]
fn outputs_and_target_kind_follow_the_dproj() {
    let fixture = fixture(Some("Debug"), Some("Win64"));
    generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();
    assert!(options.contains(" -TX.exe "), "{options}");
    assert!(options.contains(r" -E.\Win64\Debug "), "{options}");
    assert!(options.contains(r" -NU.\Win64\Debug "), "{options}");
    // rtl.dcp is a package reference; Unit1.pas is not.
    assert!(options.ends_with(" -LUrtl;"), "{options}");
}

#[test]
fn always_emits_the_switches_delphilsp_requires() {
    let fixture = fixture(None, None);
    generate(&fixture.request).unwrap();
    let options = written_settings(&fixture)["dccOptions"].as_str().unwrap().to_string();
    for switch in ["--no-config", " -Q ", " -Z "] {
        assert!(options.contains(switch), "missing {switch} in {options}");
    }
}

#[test]
fn missing_installation_is_reported_not_panicked() {
    let mut fixture = fixture(None, None);
    fixture.request.installation_path = fixture.request.installation_path.join("nope");
    let error = generate(&fixture.request).unwrap_err().to_string();
    assert!(error.contains("rsvars.bat"), "{error}");
}

#[test]
fn builder_output_is_stable_for_a_minimal_input() {
    let options = build_dcc_options(&DccOptionsInput {
        stack_frames: true,
        unit_aliases: "A=B".into(),
        defines: "DEBUG".into(),
        namespaces: "System;".into(),
        exe_output: r".\Win32\Debug".into(),
        dcu_output: r".\Win32\Debug".into(),
        search_paths: vec![r"c:\lib".into()],
        ..Default::default()
    });
    assert_eq!(
        options,
        concat!(
            r"-$O- -$W+ --no-config -Q -Z -TX.exe -AA=B -DDEBUG -E.\Win32\Debug ",
            r"-Ic:\lib -NU.\Win32\Debug -NSSystem; -Oc:\lib -Rc:\lib -Uc:\lib"
        )
    );
}
