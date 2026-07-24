use std::fs;
use std::path::PathBuf;
use ddk_core::files::dproj::read_raw_property_for;
use ddk_core::projects::{Project, ProjectUpdateData, ProjectsData};

/// Minimal package .dproj modeled on a real IDE-generated one: flag-defining
/// property groups with compound conditions, option groups with the plain
/// `'$(X)'!=''` form, and the BuildConfiguration key mapping (Release=Cfg_1,
/// Debug=Cfg_2 — the order the IDE actually emits).
fn package_dproj_xml() -> &'static str {
    r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
    <PropertyGroup>
        <ProjectGuid>{00000000-0000-0000-0000-000000000002}</ProjectGuid>
        <MainSource>TestPkg.dpk</MainSource>
        <ProjectVersion>20.3</ProjectVersion>
        <FrameworkType>VCL</FrameworkType>
        <Base>True</Base>
        <Config Condition="'$(Config)'==''">Debug</Config>
        <Platform Condition="'$(Platform)'==''">Win64</Platform>
        <TargetedPlatforms>3</TargetedPlatforms>
        <AppType>Package</AppType>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Config)'=='Base' or '$(Base)'!=''">
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Config)'=='Release' or '$(Cfg_1)'!=''">
        <Cfg_1>true</Cfg_1>
        <CfgParent>Base</CfgParent>
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Config)'=='Debug' or '$(Cfg_2)'!=''">
        <Cfg_2>true</Cfg_2>
        <CfgParent>Base</CfgParent>
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Base)'!=''">
        <DCC_BplOutput>.\$(Platform)\$(Config)</DCC_BplOutput>
        <Debugger_HostApplication>$(ProjectDir)\hosts\BaseHost.exe</Debugger_HostApplication>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Cfg_2)'!=''">
        <Debugger_HostApplication>hosts\DebugHost.exe</Debugger_HostApplication>
        <Debugger_RunParams>-testflag</Debugger_RunParams>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Cfg_2_Win64)'!=''">
        <Debugger_HostApplication>$(ProjectDir)\hosts\DebugHost64.exe</Debugger_HostApplication>
    </PropertyGroup>
    <ProjectExtensions>
        <Borland.Personality>Delphi.Personality.12</Borland.Personality>
        <Borland.ProjectType>Package</Borland.ProjectType>
        <BorlandProject>
            <Delphi.Personality>
                <Source>
                    <Source Name="MainSource">TestPkg.dpk</Source>
                </Source>
            </Delphi.Personality>
            <Platforms>
                <Platform value="Win32">True</Platform>
                <Platform value="Win64">True</Platform>
            </Platforms>
        </BorlandProject>
    </ProjectExtensions>
    <Import Project="$(BDS)\Bin\CodeGear.Delphi.Targets"/>
    <ItemGroup>
        <DelphiCompile Include="$(MainSource)">
            <MainSource>MainSource</MainSource>
        </DelphiCompile>
        <BuildConfiguration Include="Base">
            <Key>Base</Key>
        </BuildConfiguration>
        <BuildConfiguration Include="Release">
            <Key>Cfg_1</Key>
            <CfgParent>Base</CfgParent>
        </BuildConfiguration>
        <BuildConfiguration Include="Debug">
            <Key>Cfg_2</Key>
            <CfgParent>Base</CfgParent>
        </BuildConfiguration>
    </ItemGroup>
</Project>"#
}

fn write_package_fixture(dir: &std::path::Path) -> PathBuf {
    let dproj_path = dir.join("TestPkg.dproj");
    fs::write(&dproj_path, package_dproj_xml()).unwrap();
    fs::write(dir.join("TestPkg.dpk"), "package TestPkg;\nend.\n").unwrap();
    dproj_path
}

// ─── read_raw_property_for ───────────────────────────────────────────────────

#[test]
fn read_raw_property_prefers_most_specific_group() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let value = read_raw_property_for(&dproj_path, "Debug", "Win64", "Debugger_HostApplication");
    assert_eq!(value.as_deref(), Some("$(ProjectDir)\\hosts\\DebugHost64.exe"));
}

#[test]
fn read_raw_property_uses_config_group_when_no_platform_group_matches() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let value = read_raw_property_for(&dproj_path, "Debug", "Win32", "Debugger_HostApplication");
    assert_eq!(value.as_deref(), Some("hosts\\DebugHost.exe"));
}

#[test]
fn read_raw_property_falls_back_to_base_group() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let value = read_raw_property_for(&dproj_path, "Release", "Win64", "Debugger_HostApplication");
    assert_eq!(value.as_deref(), Some("$(ProjectDir)\\hosts\\BaseHost.exe"));
}

#[test]
fn read_raw_property_missing_tag_is_none() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dproj_path = write_package_fixture(tmp_dir.path());

    let value = read_raw_property_for(&dproj_path, "Debug", "Win64", "Debugger_DoesNotExist");
    assert_eq!(value, None);
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

    project.discover_paths_for("Debug", "Win64").unwrap();

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
    project.discover_paths_for("Debug", "Win32").unwrap();

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
    project.discover_paths_for("Debug", "Win64").unwrap();

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
