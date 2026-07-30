use std::fs;
use std::path::PathBuf;
use ddk_core::projects::{Project, ProjectUpdateData, ProjectsData};

/// Minimal package .dproj modeled on a real IDE-generated one: flag-defining
/// property groups with compound conditions, option groups with the plain
/// `'$(X)'!=''` form, and the BuildConfiguration key mapping (Release=Cfg_1,
/// Debug=Cfg_2 — the order the IDE actually emits).
fn package_dproj_xml() -> &'static str {
    include_str!("fixtures/TestPkg.dproj")
}

fn write_package_fixture(dir: &std::path::Path) -> PathBuf {
    let dproj_path = dir.join("TestPkg.dproj");
    fs::write(&dproj_path, package_dproj_xml()).unwrap();
    fs::write(dir.join("TestPkg.dpk"), "package TestPkg;\nend.\n").unwrap();
    dproj_path
}

// ─── property-group selection (via dproj-rs) ─────────────────────────────────

fn discover_for(dir: &std::path::Path, dproj_path: &std::path::Path, config: &str, platform: &str) -> Project {
    let mut project = Project {
        id: 1,
        name: "TestPkg".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    project.discover_paths_for(config, platform, &[]).unwrap();
    project
}

#[test]
fn discover_prefers_most_specific_group() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let project = discover_for(tmp_dir.path(), &dproj_path, "Debug", "Win64");
    let host = project.dproj_host_application.expect("host application should be discovered");
    assert!(
        host.to_lowercase().ends_with("hosts\\debughost64.exe"),
        "the Cfg_2_Win64 group must win over Cfg_2 and Base, got: {host}"
    );
}

#[test]
fn discover_uses_config_group_when_no_platform_group_matches() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let project = discover_for(tmp_dir.path(), &dproj_path, "Debug", "Win32");
    let host = project.dproj_host_application.expect("host application should be discovered");
    assert!(
        host.to_lowercase().ends_with("hosts\\debughost.exe"),
        "the Cfg_2 group must win over Base for Debug/Win32, got: {host}"
    );
}

#[test]
fn discover_falls_back_to_base_group() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let project = discover_for(tmp_dir.path(), &dproj_path, "Release", "Win64");
    let host = project.dproj_host_application.expect("host application should be discovered");
    assert!(
        host.to_lowercase().ends_with("hosts\\basehost.exe"),
        "Release must fall back to the Base group's host, got: {host}"
    );
}

#[test]
fn discover_missing_value_is_none() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    // Debugger_RunParams is only defined in the Cfg_2 (Debug) group.
    let project = discover_for(tmp_dir.path(), &dproj_path, "Release", "Win64");
    assert_eq!(project.dproj_run_params, None, "Release defines no run params");
}

// ─── discover_paths ──────────────────────────────────────────────────────────

#[test]
fn discover_paths_reads_host_application_and_run_params_for_dpk() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();
    let dproj_path = write_package_fixture(dir);

    let mut project = Project {
        id: 1,
        name: "TestPkg".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    project.discover_paths_for("Debug", "Win64", &[]).unwrap();

    assert!(project.dpk.is_some(), "package main source should be discovered");
    assert!(project.exe.is_none(), "a package has no executable");

    let host = project
        .dproj_host_application
        .clone()
        .expect("Debugger_HostApplication should be discovered for the dpk");
    assert!(
        host.to_lowercase().ends_with("debughost64.exe"),
        "expected the Debug/Win64 host, got: {host}"
    );
    assert!(
        PathBuf::from(&host).is_absolute(),
        "$(ProjectDir) must be expanded to an absolute path, got: {host}"
    );
    assert!(
        !host.contains("$("),
        "no unexpanded macros may remain, got: {host}"
    );

    assert_eq!(
        project.dproj_run_params.as_deref(),
        Some("-testflag"),
        "a package's Debugger_RunParams should be discovered too"
    );
}

#[test]
fn discover_paths_absolutizes_relative_host_application() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();
    let dproj_path = write_package_fixture(dir);

    let mut project = Project {
        id: 1,
        name: "TestPkg".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    // Debug/Win32 selects the relative "hosts\DebugHost.exe" value.
    project.discover_paths_for("Debug", "Win32", &[]).unwrap();

    let host = project
        .dproj_host_application
        .clone()
        .expect("host application should be discovered");
    assert!(
        PathBuf::from(&host).is_absolute(),
        "a relative host path must be resolved against the project directory, got: {host}"
    );
    assert!(
        host.to_lowercase().ends_with("debughost.exe"),
        "expected the Debug host, got: {host}"
    );
}

// ─── effective host + update ─────────────────────────────────────────────────

#[test]
fn effective_host_application_prefers_override_and_ignores_blanks() {
    let mut project = Project {
        dproj_host_application: Some("C:\\hosts\\FromDproj.exe".to_string()),
        ..Default::default()
    };
    assert_eq!(
        project.effective_host_application().as_deref(),
        Some("C:\\hosts\\FromDproj.exe")
    );

    project.host_application = Some("C:\\hosts\\Override.exe".to_string());
    assert_eq!(
        project.effective_host_application().as_deref(),
        Some("C:\\hosts\\Override.exe")
    );

    project.host_application = Some("   ".to_string());
    assert_eq!(
        project.effective_host_application().as_deref(),
        Some("C:\\hosts\\FromDproj.exe"),
        "a blank override must fall back to the dproj value"
    );

    project.host_application = None;
    project.dproj_host_application = None;
    assert_eq!(project.effective_host_application(), None);
}

#[test]
fn discover_paths_clears_stale_dproj_fields_for_bare_projects() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();
    std::fs::write(dir.join("standalone.dpr"), "program standalone;\nbegin\nend.\n").unwrap();

    // Simulate a project that previously had a dproj (with discovered values)
    // and lost it: the dproj-derived fields must not linger.
    let mut project = Project {
        id: 1,
        name: "standalone".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dpr: Some(dir.join("standalone.dpr").to_string_lossy().to_string()),
        dproj_run_params: Some("-stale".to_string()),
        dproj_host_application: Some("C:\\stale\\Host.exe".to_string()),
        ..Default::default()
    };
    project.discover_paths(&[]).unwrap();
    assert_eq!(project.dproj_run_params, None, "bare .dpr must clear stale run params");
    assert_eq!(project.dproj_host_application, None, "bare .dpr must clear stale host application");

    let mut package = Project {
        id: 2,
        name: "barepkg".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dpk: Some(dir.join("barepkg.dpk").to_string_lossy().to_string()),
        dproj_run_params: Some("-stale".to_string()),
        dproj_host_application: Some("C:\\stale\\Host.exe".to_string()),
        ..Default::default()
    };
    std::fs::write(dir.join("barepkg.dpk"), "package barepkg;\nend.\n").unwrap();
    package.discover_paths(&[]).unwrap();
    assert_eq!(package.dproj_run_params, None, "bare .dpk must clear stale run params");
    assert_eq!(package.dproj_host_application, None, "bare .dpk must clear stale host application");
}

#[test]
fn effective_host_application_rejects_unresolved_macros() {
    let project = Project {
        dproj_host_application: Some("$(UNDEFINED_SITE_VAR)\\Win64\\Debug\\Host.exe".to_string()),
        ..Default::default()
    };
    assert_eq!(
        project.effective_host_application(),
        None,
        "a path with an unresolved macro is not launchable and must never shadow the exe"
    );
}

#[test]
fn discover_paths_expands_environment_variable_macros() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();

    // A dproj whose host application uses a site-specific environment
    // variable, the way MSBuild resolves $(NAME) from the environment.
    unsafe {
        std::env::set_var("DDK_TEST_HOST_ROOT", dir.to_string_lossy().to_string());
    }
    let dproj_xml = package_dproj_xml().replace(
        "$(ProjectDir)\\hosts\\DebugHost64.exe",
        "$(DDK_TEST_HOST_ROOT)\\hosts\\DebugHost64.exe",
    );
    let dproj_path = dir.join("TestPkg.dproj");
    std::fs::write(&dproj_path, dproj_xml).unwrap();
    std::fs::write(dir.join("TestPkg.dpk"), "package TestPkg;\nend.\n").unwrap();

    let mut project = Project {
        id: 1,
        name: "TestPkg".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    project.discover_paths_for("Debug", "Win64", &[]).unwrap();

    let host = project
        .dproj_host_application
        .clone()
        .expect("host application should be discovered");
    assert!(
        !host.contains("$("),
        "$(DDK_TEST_HOST_ROOT) must be expanded from the environment, got: {host}"
    );
    assert!(
        host.to_lowercase().ends_with("debughost64.exe"),
        "expected the env-var-based host, got: {host}"
    );
}

#[test]
fn update_project_sets_and_clears_host_application() {
    let mut data = ProjectsData::default();
    data.projects.push(Project {
        id: 7,
        name: "TestPkg".to_string(),
        ..Default::default()
    });

    let update = |host: Option<&str>| ProjectUpdateData {
        name: None,
        directory: None,
        dproj: None,
        dpr: None,
        dpk: None,
        exe: None,
        ini: None,
        start_parameters: None,
        host_application: host.map(|s| s.to_string()),
    };

    data.update_project(7, update(Some("C:\\hosts\\MyHost.exe"))).unwrap();
    assert_eq!(
        data.get_project(7).unwrap().host_application.as_deref(),
        Some("C:\\hosts\\MyHost.exe")
    );

    // An omitted field leaves the value untouched.
    data.update_project(7, update(None)).unwrap();
    assert_eq!(
        data.get_project(7).unwrap().host_application.as_deref(),
        Some("C:\\hosts\\MyHost.exe")
    );

    // A blank value clears the override.
    data.update_project(7, update(Some("   "))).unwrap();
    assert_eq!(data.get_project(7).unwrap().host_application, None);
}

#[test]
fn discover_win64_default_platform_resolves_platform_specific_host() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();
    let dproj_path = dir.join("TestPkg64.dproj");
    fs::write(&dproj_path, include_str!("fixtures/TestPkg64.dproj")).unwrap();
    fs::write(dir.join("TestPkg.dpk"), "package TestPkg;\nend.\n").unwrap();

    let mut project = Project {
        id: 1,
        name: "TestPkg64".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let ide_env = vec![("VEGADIR".to_string(), r"c:\Athens\hydra_2".to_string())];
    project.discover_paths(&ide_env).unwrap();

    assert_eq!(project.dproj_run_params.as_deref(), Some("-flag1"));
    let host = project.dproj_host_application.clone().expect("host should be discovered for Win64 default platform");
    assert_eq!(host.to_lowercase(), r"c:\athens\hydra_2\fieldhost64.exe");
}
