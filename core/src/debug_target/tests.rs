//! Tests of the debug target builder. Hermetic: the IDE inputs are values
//! ([`FakeIde`]), the projects live in temporary directories, and nothing
//! reads the process environment, a Delphi installation or the registry.

use super::*;
use std::collections::HashMap;
use std::fs;

/// A Delphi 12 installation that exists only as values.
struct FakeIde {
    environment: Option<IdeEnvironment>,
    library: IdeLibrarySettings,
}

impl FakeIde {
    /// An installation whose directories do not exist on this machine.
    fn new() -> Self {
        Self::rooted_at(Path::new(r"C:\ddk-test-no-such-installation"))
    }

    /// An installation under `root`: `$(BDS)` is `root\bds`,
    /// `$(BDSCOMMONDIR)` is `root\common`.
    fn rooted_at(root: &Path) -> Self {
        let rsvars: HashMap<String, String> = [("BDS", root.join("bds")), ("BDSCOMMONDIR", root.join("common"))]
            .iter()
            .map(|(name, dir)| (name.to_string(), dir.to_string_lossy().to_string()))
            .collect();
        FakeIde {
            environment: Some(IdeEnvironment { rsvars, ..Default::default() }),
            library: IdeLibrarySettings::default(),
        }
    }

    fn with_variable(mut self, name: &str, value: &str) -> Self {
        if let Some(environment) = &mut self.environment {
            environment.ide_variables.push((name.to_string(), value.to_string()));
        }
        self
    }

    fn with_library(mut self, library: IdeLibrarySettings) -> Self {
        self.library = library;
        self
    }

    fn unavailable() -> Self {
        FakeIde { environment: None, library: IdeLibrarySettings::default() }
    }
}

impl IdeSettings for FakeIde {
    fn environment(&self) -> Result<IdeEnvironment> {
        self.environment.clone().ok_or_else(|| anyhow::anyhow!("rsvars.bat not found"))
    }

    fn library_settings(&self, _platform: &str) -> IdeLibrarySettings {
        self.library.clone()
    }
}

fn compiler() -> CompilerConfiguration {
    CompilerConfiguration {
        condition: "VER360".into(),
        product_name: "Delphi 12.0 Athens".into(),
        product_version: 23,
        package_version: 290,
        compiler_version: 36,
        installation_path: r"C:\ddk-test-no-such-installation\bds".into(),
        build_arguments: Vec::new(),
    }
}

fn project(dir: &Path, name: &str) -> Project {
    Project { id: 7, name: name.into(), directory: dir.to_string_lossy().to_string(), ..Default::default() }
}

fn text(path: PathBuf) -> String {
    path.to_string_lossy().to_string()
}

/// Writes a non-empty file, creating its directory.
fn touch(path: PathBuf) -> String {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"x").unwrap();
    text(path)
}

fn directory(path: PathBuf) -> String {
    fs::create_dir_all(&path).unwrap();
    text(path)
}

/// The path as the target reports it.
fn reported(path: impl AsRef<Path>) -> String {
    json_path(&normalize_path(path).to_string_lossy())
}

fn set_modified(path: &str, time: SystemTime) {
    fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
}

fn describe(project: &Project, ide: &FakeIde) -> DebugTarget {
    build_debug_target_with(project, &compiler(), ide).unwrap()
}

fn warns(target: &DebugTarget, fragments: &[&str]) -> bool {
    target.warnings.iter().any(|warning| fragments.iter().all(|fragment| warning.contains(fragment)))
}

fn notes(target: &DebugTarget, fragments: &[&str]) -> bool {
    target.notes.iter().any(|note| fragments.iter().all(|fragment| note.contains(fragment)))
}

/// A project built from a fixture dproj copied as `<name>.dproj`, with an
/// empty main source `<main_source>` next to it.
fn project_from_fixture(dir: &Path, name: &str, fixture: &str, main_source: &str) -> Project {
    let dproj = dir.join(format!("{name}.dproj"));
    fs::write(&dproj, fixture).unwrap();
    let main_source_path = touch(dir.join(main_source));
    let mut project = project(dir, name);
    project.dproj = Some(text(dproj));
    if main_source.to_lowercase().ends_with(".dpk") {
        project.dpk = Some(main_source_path);
    } else {
        project.dpr = Some(main_source_path);
    }
    project
}

const TEST_PKG_64: &str = include_str!("../../tests/fixtures/TestPkg64.dproj");
const TEST_PKG_SUFFIX: &str = include_str!("../../tests/fixtures/TestPkgSuffix.dproj");
const TEST_LIB: &str = include_str!("../../tests/fixtures/TestLib.dproj");

/// A package `Demo.dpk` without a dproj, hosted by an existing `Host.exe`.
fn hosted_package(dir: &Path) -> Project {
    let mut project = project(dir, "Demo");
    project.dpk = Some(touch(dir.join("Demo.dpk")));
    project.host_application = Some(touch(dir.join("host").join("Host.exe")));
    project
}

// ─── Programs ────────────────────────────────────────────────────────────────

#[test]
fn a_built_program_with_its_symbols_and_sources_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut project = project(&root.join("app"), "Demo");
    project.dpr = Some(touch(root.join("app").join("Demo.dpr")));
    project.exe = Some(touch(root.join("app").join("Demo.exe")));
    touch(root.join("app").join("Demo.map"));
    touch(root.join("app").join("Demo.rsm"));
    project.dproj_run_params = Some("-a".into());
    project.start_parameters = Some("\"b c\"".into());
    let ide = FakeIde::rooted_at(root).with_library(IdeLibrarySettings {
        search_path: Some(format!("{};$(BDS)\\lib", directory(root.join("library")))),
        browsing_path: Some(directory(root.join("browsing"))),
        ..Default::default()
    });
    directory(root.join("bds").join("lib"));
    directory(root.join("bds").join("source"));

    let target = describe(&project, &ide);
    assert_eq!(target.warnings, Vec::<String>::new());
    assert_eq!(target.notes, Vec::<String>::new());
    assert_eq!(target.kind, DebugTargetKind::Program);
    assert_eq!(target.bitness, Some(32));
    assert_eq!(target.executable, reported(root.join("app").join("Demo.exe")));
    assert_eq!(target.host_application, None);
    assert_eq!(target.symbols.map, Some(reported(root.join("app").join("Demo.map"))));
    assert_eq!(target.symbols.rsm, Some(reported(root.join("app").join("Demo.rsm"))));
    assert_eq!(target.args, vec!["-a", "b c"]);
    assert!(target.modules.is_empty());
    assert_eq!(
        target.source_search_paths,
        vec![
            reported(root.join("app")),
            reported(root.join("library")),
            reported(root.join("bds").join("lib")),
            reported(root.join("browsing")),
            reported(root.join("bds").join("source")),
        ]
    );
}

#[test]
fn a_program_without_symbols_offers_none_and_says_what_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.exe = Some(touch(tmp.path().join("Demo.exe")));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.symbols, SymbolFiles::default());
    assert!(warns(&target, &["Missing .map", "the executable"]));
    assert!(warns(&target, &["Missing .rsm", "the executable"]));
    assert!(notes(&target, &["No IDE Library Path"]));
    assert!(notes(&target, &["No IDE Browsing Path"]));
}

#[test]
fn a_program_never_built_is_described_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.exe = Some(text(tmp.path().join("Win32").join("Debug").join("Demo.exe")));

    let target = describe(&project, &FakeIde::new());
    assert!(target.executable.ends_with("/Win32/Debug/Demo.exe"));
    assert_eq!(target.symbols, SymbolFiles::default());
    assert!(warns(&target, &["Executable not found"]));
    assert!(!warns(&target, &["Missing .map"]), "one problem, one warning: {:?}", target.warnings);
}

#[test]
fn a_program_with_a_host_application_keeps_its_own_symbols_as_a_module() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Plugin");
    project.exe = Some(touch(tmp.path().join("out").join("Plugin.exe")));
    touch(tmp.path().join("out").join("Plugin.map"));
    project.host_application = Some(touch(tmp.path().join("Host.exe")));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.kind, DebugTargetKind::Program);
    assert_eq!(target.executable, reported(tmp.path().join("Host.exe")));
    // The host's symbols are a bonus: none are there, none are missed.
    assert_eq!(target.symbols, SymbolFiles::default());
    assert!(!warns(&target, &["the executable"]), "{:?}", target.warnings);
    let module = &target.modules[0];
    assert_eq!(module.name, "Plugin.exe");
    assert_eq!(module.binary, Some(reported(tmp.path().join("out").join("Plugin.exe"))));
    assert_eq!(module.map, Some(reported(tmp.path().join("out").join("Plugin.map"))));
    assert_eq!(module.rsm, None);
    assert!(warns(&target, &["Missing .rsm", "Plugin.exe"]));
}

#[test]
fn a_hosted_program_without_an_executable_of_its_own_is_an_unbuilt_module() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Shown As");
    project.dpr = Some(touch(tmp.path().join("Plugin.dpr")));
    project.host_application = Some(touch(tmp.path().join("Host.exe")));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.modules, vec![unbuilt_module("Plugin.exe".into())]);
    assert!(warns(&target, &["no executable of its own"]));
}

#[test]
fn a_program_with_neither_executable_nor_host_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let error = build_debug_target_with(&project(tmp.path(), "Demo"), &compiler(), &FakeIde::new()).unwrap_err();
    assert!(error.to_string().contains("no executable to debug"), "{error}");
}

// ─── Platform ────────────────────────────────────────────────────────────────

#[test]
fn bitness_follows_the_platform_and_a_foreign_platform_is_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.exe = Some(touch(tmp.path().join("Demo.exe")));
    for (platform, bitness) in [("Win32", Some(32)), ("Win64", Some(64)), ("Win64x", Some(64)), ("Android64", None)] {
        project.active_platform = Some(platform.into());
        let target = describe(&project, &FakeIde::new());
        assert_eq!(target.bitness, bitness, "{platform}");
        assert_eq!(warns(&target, &["not a Windows target"]), bitness.is_none(), "{platform}");
    }
}

#[test]
fn a_platform_the_project_does_not_enable_is_a_warning_and_changes_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project_from_fixture(tmp.path(), "TestLib", TEST_LIB, "TestLib.dpr");
    touch(tmp.path().join("Host.exe"));

    let enabled = describe(&project, &FakeIde::new());
    assert_eq!(enabled.kind, DebugTargetKind::Library);
    assert!(!warns(&enabled, &["not enabled"]), "{:?}", enabled.warnings);

    project.active_platform = Some("Win64".into());
    let foreign = describe(&project, &FakeIde::new());
    assert!(warns(&foreign, &["Platform Win64 is not enabled", "Win32"]), "{:?}", foreign.warnings);
    // The kind comes from the unconditional group, not from the platform's.
    assert_eq!(foreign.kind, DebugTargetKind::Library);
}

// ─── Host application ────────────────────────────────────────────────────────

#[test]
fn a_relative_host_override_is_taken_from_the_project_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = hosted_package(tmp.path());
    touch(tmp.path().join("hosts").join("Host.exe"));
    project.host_application = Some(r"hosts\Host.exe".into());

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.executable, reported(tmp.path().join("hosts").join("Host.exe")));
    assert!(!warns(&target, &["Executable not found"]), "{:?}", target.warnings);
}

#[test]
fn a_host_depending_on_an_undefined_variable_is_no_host() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = hosted_package(tmp.path());
    project.host_application = Some(r"$(NOWHERE_DDK)\Host.exe".into());
    project.dproj_host_application = Some(touch(tmp.path().join("recorded").join("Host.exe")));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.executable, reported(tmp.path().join("recorded").join("Host.exe")));
    assert!(warns(&target, &["Host Application override", "$(NOWHERE_DDK)"]), "{:?}", target.warnings);
}

#[test]
fn a_host_with_symbols_of_its_own_offers_them() {
    let tmp = tempfile::tempdir().unwrap();
    let project = hosted_package(tmp.path());
    touch(tmp.path().join("host").join("Host.map"));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.symbols.map, Some(reported(tmp.path().join("host").join("Host.map"))));
    assert_eq!(target.symbols.rsm, None);
    assert!(!warns(&target, &["the executable"]), "{:?}", target.warnings);
}

#[test]
fn a_package_without_host_is_an_error_not_a_target() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.dpk = Some(text(tmp.path().join("Demo.dpk")));
    let error = build_debug_target_with(&project, &compiler(), &FakeIde::new()).unwrap_err().to_string();
    assert!(error.contains("Host Application"), "{error}");
}

// ─── Packages and libraries ──────────────────────────────────────────────────

#[test]
fn a_package_target_resolves_host_platform_and_search_paths_from_the_dproj() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_from_fixture(tmp.path(), "TestPkg64", TEST_PKG_64, "TestPkg.dpk");
    directory(tmp.path().join("Win64").join("Debug"));
    let ide = FakeIde::new().with_variable("VEGADIR", r"C:\ddk-test-vega");

    // Nothing recorded about the host: the dproj is read, for its default
    // platform (Win64), with the IDE's variable expanded.
    let target = describe(&project, &ide);
    assert_eq!(target.kind, DebugTargetKind::Package);
    assert_eq!((target.config.as_str(), target.platform.as_str(), target.bitness), ("Debug", "Win64", Some(64)));
    assert_eq!(target.executable.to_lowercase(), "c:/ddk-test-vega/fieldhost64.exe");
    assert_eq!(target.host_application, Some(target.executable.clone()));
    // Named after the main source (TestPkg.dpk), not after the project.
    assert_eq!(target.modules, vec![unbuilt_module("TestPkg.bpl".into())]);
    assert!(target.source_search_paths.contains(&reported(tmp.path().join("Win64").join("Debug"))));
    assert!(warns(&target, &["Executable not found"]));
    assert!(warns(&target, &["No built TestPkg.bpl"]));
}

#[test]
fn a_package_uses_the_recorded_state_and_finds_its_bpl_next_to_the_host() {
    let tmp = tempfile::tempdir().unwrap();
    let vega = tmp.path().join("vega");
    let mut project = project_from_fixture(tmp.path(), "TestPkg64", TEST_PKG_64, "TestPkg.dpk");
    project.dproj_host_application = Some(touch(vega.join("FieldHost64.exe")));
    project.dproj_run_params = Some("-flag1".into());
    touch(vega.join("TestPkg.bpl"));
    touch(vega.join("TestPkg.dcp"));

    let target = describe(&project, &FakeIde::new().with_variable("VEGADIR", &text(vega.clone())));
    assert_eq!(target.executable, reported(vega.join("FieldHost64.exe")));
    assert_eq!(target.args, vec!["-flag1"]);
    let module = &target.modules[0];
    assert_eq!(module.binary, Some(reported(vega.join("TestPkg.bpl"))));
    assert_eq!(module.dcp, Some(reported(vega.join("TestPkg.dcp"))));
    assert_eq!((module.map.clone(), module.rsm.clone()), (None, None));
    assert!(warns(&target, &["TestPkg.bpl", "Missing .map"]));
}

#[test]
fn a_package_is_named_after_its_main_source_whatever_the_project_is_called() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = hosted_package(tmp.path());
    project.name = "Shown As Something Else".into();
    touch(tmp.path().join("host").join("Demo.bpl"));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.modules[0].name, "Demo.bpl");
    assert!(target.modules[0].binary.is_some());
}

#[test]
fn a_package_with_an_automatic_libsuffix_is_found_by_its_exact_name_in_the_bpl_output() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_from_fixture(tmp.path(), "TestPkgSuffix", TEST_PKG_SUFFIX, "TestPkgSuffix.dpk");
    touch(tmp.path().join("hosts").join("Host.exe"));
    touch(tmp.path().join("bpl").join("TestPkgSuffix290.bpl"));
    touch(tmp.path().join("dcp").join("TestPkgSuffix.dcp"));
    directory(tmp.path().join("inc"));
    // A namesake a prefix match would prefer on both counts: it sorts first
    // ('-' < '2') and it is the newer file.
    let decoy = touch(tmp.path().join("bpl").join("TestPkgSuffix-old.bpl"));
    set_modified(&decoy, SystemTime::now() + Duration::from_secs(60));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.executable, reported(tmp.path().join("hosts").join("Host.exe")));
    let module = &target.modules[0];
    assert_eq!(module.name, "TestPkgSuffix290.bpl");
    assert_eq!(module.binary, Some(reported(tmp.path().join("bpl").join("TestPkgSuffix290.bpl"))));
    assert_eq!(module.dcp, Some(reported(tmp.path().join("dcp").join("TestPkgSuffix.dcp"))));
    assert!(target.source_search_paths.contains(&reported(tmp.path().join("inc"))));
}

#[test]
fn the_libsuffix_a_dpk_declares_is_the_one_for_this_compiler() {
    let tmp = tempfile::tempdir().unwrap();
    let project = hosted_package(tmp.path());
    let dpk = project.dpk.clone().unwrap();
    let name_for = |source: &[u8]| {
        fs::write(&dpk, source).unwrap();
        describe(&project, &FakeIde::new())
    };

    assert_eq!(name_for(b"package Demo;\n{$LIBSUFFIX 'D29'}\nend.").modules[0].name, "DemoD29.bpl");
    assert_eq!(name_for(b"package Demo;\n{$LIBSUFFIX AUTO}\nend.").modules[0].name, "Demo290.bpl");
    let per_version = b"package Demo;\n{$IFDEF VER350}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}\nend.";
    assert_eq!(name_for(per_version).modules[0].name, "Demo290.bpl");
    // A Windows-1252 source, an umlaut in a comment: not valid UTF-8, still read.
    assert_eq!(name_for(b"package Demo; // f\xFCr Delphi\n{$LIBSUFFIX '290'}\nend.").modules[0].name, "Demo290.bpl");

    let undecidable = name_for(b"package Demo;\n{$IFOPT D+}{$LIBSUFFIX 'D'}{$ELSE}{$LIBSUFFIX 'R'}{$ENDIF}\nend.");
    assert_eq!(undecidable.modules[0].name, "Demo.bpl");
    assert!(warns(&undecidable, &["several {$LIBSUFFIX} values", "'D', 'R'"]), "{:?}", undecidable.warnings);
}

#[test]
fn a_package_is_found_in_the_ides_default_output_of_its_platform_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut project = hosted_package(&root.join("pkg"));
    let ide = FakeIde::rooted_at(root);
    // Only the Win32 build exists, where the IDE puts Win32 packages.
    let win32_bpl = touch(root.join("common").join("Bpl").join("Demo.bpl"));
    touch(root.join("common").join("Dcp").join("Demo.dcp"));

    project.active_platform = Some("Win32".into());
    let win32 = describe(&project, &ide);
    assert_eq!(win32.modules[0].binary, Some(reported(&win32_bpl)));
    assert_eq!(win32.modules[0].dcp, Some(reported(root.join("common").join("Dcp").join("Demo.dcp"))));

    project.active_platform = Some("Win64".into());
    let win64 = describe(&project, &ide);
    assert_eq!(win64.modules[0].binary, None, "a Win32 package is not the Win64 build");
    assert!(warns(&win64, &["No built Demo.bpl"]));

    let win64_bpl = touch(root.join("common").join("Bpl").join("Win64").join("Demo.bpl"));
    assert_eq!(describe(&project, &ide).modules[0].binary, Some(reported(&win64_bpl)));
}

#[test]
fn a_package_is_found_in_the_output_directories_the_ide_is_configured_with() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let project = hosted_package(&root.join("pkg"));
    let ide = FakeIde::rooted_at(root).with_library(IdeLibrarySettings {
        package_dpl_output: Some(r"$(BDSCOMMONDIR)\MyBpl".into()),
        package_dcp_output: Some(text(root.join("my dcp"))),
        ..Default::default()
    });
    touch(root.join("common").join("MyBpl").join("Demo.bpl"));
    touch(root.join("my dcp").join("Demo.dcp"));

    let target = describe(&project, &ide);
    assert_eq!(target.modules[0].binary, Some(reported(root.join("common").join("MyBpl").join("Demo.bpl"))));
    assert_eq!(target.modules[0].dcp, Some(reported(root.join("my dcp").join("Demo.dcp"))));
}

#[test]
fn a_package_is_found_in_the_platform_and_configuration_directory_of_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = hosted_package(tmp.path());
    project.active_platform = Some("Win64".into());
    project.active_configuration = Some("Release".into());
    let bpl = touch(tmp.path().join("Win64").join("Release").join("Demo.bpl"));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.modules[0].binary, Some(reported(&bpl)));
    // The `.dcp` is looked for next to the `.bpl` too.
    assert!(warns(&target, &["No Demo.dcp found"]));
}

#[test]
fn a_different_copy_in_the_hosts_directory_is_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let project = hosted_package(tmp.path());
    touch(tmp.path().join("Win32").join("Debug").join("Demo.bpl"));
    fs::write(tmp.path().join("host").join("Demo.bpl"), b"an older, longer build").unwrap();

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.modules[0].binary, Some(reported(tmp.path().join("Win32").join("Debug").join("Demo.bpl"))));
    assert!(warns(&target, &["is a different file", "the one the host loads"]), "{:?}", target.warnings);
}

#[test]
fn a_library_target_binds_its_dll() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project_from_fixture(tmp.path(), "TestLib", TEST_LIB, "TestLib.dpr");
    // DevKit records a library's output the way it records a program's.
    project.exe = Some(text(tmp.path().join("out").join("TestLib.exe")));
    touch(tmp.path().join("Host.exe"));
    touch(tmp.path().join("out").join("TestLib.dll"));
    touch(tmp.path().join("out").join("TestLib.rsm"));

    let target = describe(&project, &FakeIde::new());
    assert_eq!(target.kind, DebugTargetKind::Library, "{:?}", target.warnings);
    assert_eq!(target.executable, reported(tmp.path().join("Host.exe")));
    let module = &target.modules[0];
    assert_eq!(module.name, "TestLib.dll");
    assert_eq!(module.binary, Some(reported(tmp.path().join("out").join("TestLib.dll"))));
    assert_eq!(module.rsm, Some(reported(tmp.path().join("out").join("TestLib.rsm"))));
    assert_eq!(module.map, None);
}

// ─── Inputs that cannot be used ──────────────────────────────────────────────

#[test]
fn unreadable_inputs_are_reported_not_swallowed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.exe = Some(touch(tmp.path().join("Demo.exe")));
    let dproj = tmp.path().join("Demo.dproj");
    fs::write(&dproj, "<Project><PropertyGroup><Config>").unwrap();
    project.dproj = Some(text(dproj));

    let target = describe(&project, &FakeIde::unavailable());
    assert!(warns(&target, &["rsvars.bat not found"]), "{:?}", target.warnings);
    assert!(warns(&target, &["Could not evaluate"]), "{:?}", target.warnings);
}

#[test]
fn a_dproj_value_depending_on_an_undefined_variable_is_reported_and_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = TEST_PKG_SUFFIX
        .replace(r"<DCC_BplOutput>.\bpl</DCC_BplOutput>", r"<DCC_BplOutput>$(NOWHERE_DDK)\bpl</DCC_BplOutput>")
        .replace(r".\inc;$(DCC_IncludePath)", r"$(NOWHERE_DDK)\inc;$(DCC_IncludePath)");
    let project = project_from_fixture(tmp.path(), "TestPkgSuffix", &fixture, "TestPkgSuffix.dpk");
    touch(tmp.path().join("hosts").join("Host.exe"));

    let target = describe(&project, &FakeIde::new());
    assert!(warns(&target, &["DCC_BplOutput", "$(NOWHERE_DDK)"]), "{:?}", target.warnings);
    assert!(warns(&target, &["source search path entries", "$(NOWHERE_DDK)", "include path"]), "{:?}", target.warnings);
    assert!(
        !target.source_search_paths.iter().any(|path| path.to_lowercase().ends_with("/inc")),
        "{:?}",
        target.source_search_paths
    );
}

#[test]
fn search_path_entries_that_do_not_exist_are_left_out_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "Demo");
    project.exe = Some(touch(tmp.path().join("Demo.exe")));
    let ide = FakeIde::new().with_library(IdeLibrarySettings {
        search_path: Some(r"C:\ddk-test-gone\lib".into()),
        browsing_path: Some(r"C:\ddk-test-gone\src".into()),
        ..Default::default()
    });

    let target = describe(&project, &ide);
    assert_eq!(target.source_search_paths, vec![reported(tmp.path())]);
    assert!(notes(&target, &["do not exist", r"C:\ddk-test-gone\lib", r"C:\ddk-test-gone\src"]), "{:?}", target.notes);
    assert!(!notes(&target, &["No IDE Library Path"]), "{:?}", target.notes);
}

#[test]
fn a_project_without_any_source_directory_is_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(&tmp.path().join("gone"), "Demo");
    project.exe = Some(touch(tmp.path().join("Demo.exe")));

    let target = describe(&project, &FakeIde::new());
    assert!(target.source_search_paths.is_empty());
    assert!(warns(&target, &["No source directory exists"]));
}

// ─── Symbol files ────────────────────────────────────────────────────────────

/// A program whose `.map` was last written `offset` seconds from its exe.
fn program_with_map_written(dir: &Path, offset: i64, map_content: &[u8]) -> DebugTarget {
    let mut project = project(dir, "Demo");
    let exe = touch(dir.join("Demo.exe"));
    let map = text(dir.join("Demo.map"));
    fs::write(&map, map_content).unwrap();
    touch(dir.join("Demo.rsm"));
    let built = SystemTime::now() - Duration::from_secs(24 * 3600);
    let shift = Duration::from_secs(offset.unsigned_abs());
    set_modified(&exe, built);
    set_modified(&text(dir.join("Demo.rsm")), built);
    set_modified(&map, if offset < 0 { built - shift } else { built + shift });
    project.exe = Some(exe);
    describe(&project, &FakeIde::new())
}

#[test]
fn symbols_written_within_the_same_build_are_offered() {
    let tmp = tempfile::tempdir().unwrap();
    for offset in [-60, 0, 60] {
        let target = program_with_map_written(tmp.path(), offset, b"map");
        assert!(target.symbols.map.is_some(), "{offset}: {:?}", target.warnings);
        assert_eq!(target.warnings, Vec::<String>::new(), "{offset}");
    }
}

#[test]
fn symbols_of_another_build_are_reported_and_withheld() {
    let tmp = tempfile::tempdir().unwrap();
    let older = program_with_map_written(tmp.path(), -3600, b"map");
    assert_eq!(older.symbols.map, None);
    assert!(older.symbols.rsm.is_some());
    assert!(warns(&older, &[".map", "older than the binary"]), "{:?}", older.warnings);

    let newer = program_with_map_written(tmp.path(), 3600, b"map");
    assert_eq!(newer.symbols.map, None);
    assert!(warns(&newer, &[".map", "newer than the binary"]), "{:?}", newer.warnings);
}

#[test]
fn an_empty_symbol_file_is_reported_and_withheld() {
    let tmp = tempfile::tempdir().unwrap();
    let target = program_with_map_written(tmp.path(), 0, b"");
    assert_eq!(target.symbols.map, None);
    assert!(warns(&target, &[".map", "is empty"]), "{:?}", target.warnings);
}

#[test]
fn a_file_time_that_cannot_be_compared_rules_nothing_out() {
    assert_eq!(written_by_another_build(None, Some(SystemTime::now())), None);
    assert_eq!(written_by_another_build(Some(SystemTime::UNIX_EPOCH), None), None);
    // The zero of the file system's clock: no arithmetic on it may panic.
    assert_eq!(written_by_another_build(Some(SystemTime::UNIX_EPOCH), Some(SystemTime::UNIX_EPOCH)), None);
    assert!(written_by_another_build(Some(SystemTime::UNIX_EPOCH), Some(SystemTime::now())).is_some());
}

// ─── Another build than the active one ───────────────────────────────────────

const APP: &str = include_str!("../../tests/fixtures/App.dproj");

#[test]
fn describing_another_build_discovers_the_executable_of_that_build() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project_from_fixture(tmp.path(), "App", APP, "App.dpr");
    project.discover_paths(&[]).unwrap();
    let active_exe = project.exe.clone().unwrap();
    assert!(active_exe.to_lowercase().ends_with(r"\win64\debug\app.exe"), "{active_exe}");

    let (described, warnings) = project_to_describe(&project, Some("Release".into()), Some("Win32".into()), &[]);
    assert_eq!(warnings, Vec::<String>::new());
    assert!(described.exe.as_deref().unwrap().to_lowercase().ends_with(r"\win32\release\app.exe"), "{:?}", described.exe);
    assert_eq!(project.exe, Some(active_exe.clone()), "the managed project is never touched");

    let target = describe(&described, &FakeIde::new());
    assert_eq!((target.config.as_str(), target.platform.as_str(), target.bitness), ("Release", "Win32", Some(32)));
    assert!(target.executable.to_lowercase().ends_with("/win32/release/app.exe"), "{}", target.executable);
    assert!(warns(&target, &["Executable not found"]));

    let (unchanged, warnings) = project_to_describe(&project, None, None, &[]);
    assert_eq!((unchanged.exe, warnings), (Some(active_exe), Vec::new()));
}

#[test]
fn a_build_that_cannot_be_discovered_does_not_borrow_the_active_ones_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = project(tmp.path(), "App");
    project.dproj = Some(text(tmp.path().join("Gone.dproj")));
    project.exe = Some(text(tmp.path().join("Win32").join("Debug").join("App.exe")));
    project.dproj_host_application = Some(text(tmp.path().join("Host.exe")));

    let (described, warnings) = project_to_describe(&project, None, Some("Win64".into()), &[]);
    assert_eq!((described.exe, described.dproj_host_application), (None, None));
    assert!(warnings[0].contains("could not be discovered"), "{warnings:?}");
}

// ─── What callers see ────────────────────────────────────────────────────────

#[test]
fn the_text_form_lists_the_target_with_its_warnings_and_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let project = hosted_package(tmp.path());
    let printed = describe(&project, &FakeIde::new()).to_string();
    for expected in [
        "Debug target for \"Demo\" (package, Debug Win32, Delphi 12.0 Athens):",
        "  host app:     ",
        "  map / rsm:    (none) / (none)",
        "  module:       Demo.bpl -> (not built)",
        "Warnings:\n- No built Demo.bpl",
        "Notes:\n- No IDE Library Path",
    ] {
        assert!(printed.contains(expected), "missing {expected:?} in:\n{printed}");
    }
}

/// The JSON a debugger integration reads. Its field names and the spelling
/// of `kind` are a contract with code outside this repository (and with the
/// `DebugTarget` interface of the VS Code extension, checked against the
/// same sample): a change here is a change for them.
#[test]
fn the_json_form_is_the_published_contract() {
    let target = DebugTarget {
        project_id: Some(7),
        project: "Demo".into(),
        project_file: "C:/src/Demo.dproj".into(),
        main_source: Some("C:/src/Demo.dpk".into()),
        kind: DebugTargetKind::Package,
        executable: "C:/host/Host.exe".into(),
        host_application: Some("C:/host/Host.exe".into()),
        compiler: "Delphi 12.0 Athens".into(),
        config: "Debug".into(),
        platform: "Win64".into(),
        bitness: Some(64),
        symbols: SymbolFiles { map: Some("C:/host/Host.map".into()), rsm: None },
        source_root: "C:/src".into(),
        source_search_paths: vec!["C:/src".into()],
        modules: vec![DebugModule {
            name: "Demo290.bpl".into(),
            binary: Some("C:/bpl/Demo290.bpl".into()),
            map: Some("C:/bpl/Demo290.map".into()),
            rsm: Some("C:/bpl/Demo290.rsm".into()),
            dcp: None,
        }],
        args: vec!["-a".into()],
        warnings: vec!["No Demo.dcp found".into()],
        notes: vec!["No IDE Browsing Path is configured".into()],
    };
    let sample: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/debug_target.sample.json")).unwrap();
    assert_eq!(serde_json::to_value(&target).unwrap(), sample);
    for (kind, spelling) in [
        (DebugTargetKind::Program, "program"),
        (DebugTargetKind::Package, "package"),
        (DebugTargetKind::Library, "library"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(spelling));
    }
}
