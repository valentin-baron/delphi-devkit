use std::fs;
use std::path::PathBuf;
use ddk_core::projects::Project;

/// Minimal .dproj XML modeled after beas.test.external.dproj.
/// The key aspects:
/// - MainSource = example.debug.test.DPR
/// - DCC_ExeOutput = .  (output in same directory)
/// - ProjectName = example.debug.test
fn sample_dproj_xml() -> &'static str {
    r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
    <PropertyGroup>
        <ProjectGuid>{00000000-0000-0000-0000-000000000001}</ProjectGuid>
        <MainSource>example.debug.test.DPR</MainSource>
        <DCC_DependencyCheckOutputName>.\example.debug.test.exe</DCC_DependencyCheckOutputName>
        <Configuration Condition=" '$(Configuration)' == '' ">Debug</Configuration>
        <DCC_DCCCompiler>DCC32</DCC_DCCCompiler>
        <FrameworkType>VCL</FrameworkType>
        <ProjectVersion>20.3</ProjectVersion>
        <Base>True</Base>
        <Config Condition="'$(Config)'==''">Debug</Config>
        <Platform Condition="'$(Platform)'==''">Win32</Platform>
        <TargetedPlatforms>1</TargetedPlatforms>
        <AppType>Application</AppType>
        <ProjectName Condition="'$(ProjectName)'==''">example.debug.test</ProjectName>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Config)'=='Base' or '$(Base)'!=''">
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''">
        <Base_Win32>true</Base_Win32>
        <CfgParent>Base</CfgParent>
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Config)'=='Debug' or '$(Cfg_2)'!=''">
        <Cfg_2>true</Cfg_2>
        <CfgParent>Base</CfgParent>
        <Base>true</Base>
    </PropertyGroup>
    <PropertyGroup Condition="'$(Base)'!=''">
        <DCC_ExeOutput>.</DCC_ExeOutput>
    </PropertyGroup>
    <ProjectExtensions>
        <Borland.Personality>Delphi.Personality.12</Borland.Personality>
        <Borland.ProjectType>VCLApplication</Borland.ProjectType>
        <BorlandProject>
            <Delphi.Personality>
                <Source>
                    <Source Name="MainSource">example.debug.test.DPR</Source>
                </Source>
            </Delphi.Personality>
            <Platforms>
                <Platform value="Win32">True</Platform>
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
        <BuildConfiguration Include="Debug">
            <Key>Cfg_2</Key>
            <CfgParent>Base</CfgParent>
        </BuildConfiguration>
    </ItemGroup>
</Project>"#
}

#[test]
fn discover_paths_resolves_exe_from_dproj_name() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();

    // Write the .dproj file
    let dproj_path = dir.join("example.debug.test.dproj");
    fs::write(&dproj_path, sample_dproj_xml()).unwrap();

    // Write a minimal .dpr file (must exist for MainSource resolution)
    let dpr_path = dir.join("example.debug.test.DPR");
    fs::write(&dpr_path, "program example_debug_test;\nbegin\nend.\n").unwrap();

    let mut project = Project {
        id: 1,
        name: "example.debug.test".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dproj: Some(dproj_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    project.discover_paths().unwrap();

    // The exe must end with example.debug.test.exe
    let exe = project.exe.expect("exe should be set after discover_paths");
    let exe_path = PathBuf::from(&exe);
    assert_eq!(
        exe_path.file_name().unwrap().to_str().unwrap(),
        "example.debug.test.exe",
        "Expected exe to be 'example.debug.test.exe', got: {}",
        exe
    );

    // INI should match
    let ini = project.ini.expect("ini should be set after discover_paths");
    let ini_path = PathBuf::from(&ini);
    assert_eq!(
        ini_path.file_name().unwrap().to_str().unwrap(),
        "example.debug.test.ini",
    );
}

#[test]
fn discover_paths_resolves_exe_for_bare_dpr_without_dproj() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();

    // Only a .dpr exists — no sibling .dproj. This is a valid project.
    let dpr_path = dir.join("standalone.dpr");
    fs::write(&dpr_path, "program standalone;\nbegin\nend.\n").unwrap();

    let mut project = Project {
        id: 1,
        name: "standalone".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dpr: Some(dpr_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    // Must succeed (previously bailed with "DPROJ file not found").
    project.discover_paths().unwrap();

    // No .dproj should have been invented.
    assert!(project.dproj.is_none(), "bare .dpr must not gain a .dproj");

    // exe/ini are derived directly from the source name.
    let exe = project.exe.expect("exe should be set for a bare .dpr");
    assert_eq!(
        PathBuf::from(&exe).file_name().unwrap().to_str().unwrap(),
        "standalone.exe",
    );
    let ini = project.ini.expect("ini should be set for a bare .dpr");
    assert_eq!(
        PathBuf::from(&ini).file_name().unwrap().to_str().unwrap(),
        "standalone.ini",
    );
}

#[test]
fn discover_paths_bare_dpk_has_no_exe() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let dir = tmp_dir.path();

    // Only a .dpk exists — a package has no standalone executable.
    let dpk_path = dir.join("mypackage.dpk");
    fs::write(&dpk_path, "package mypackage;\nend.\n").unwrap();

    let mut project = Project {
        id: 1,
        name: "mypackage".to_string(),
        directory: dir.to_string_lossy().to_string(),
        dpk: Some(dpk_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    project.discover_paths().unwrap();

    assert!(project.dproj.is_none(), "bare .dpk must not gain a .dproj");
    assert!(project.exe.is_none(), "a package has no executable");
    assert!(project.ini.is_none(), "a package has no ini");
}
