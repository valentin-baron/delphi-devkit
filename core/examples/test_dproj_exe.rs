use dproj_rs::Dproj;
use std::fs;

fn main() {
    let dir = std::env::temp_dir().join("ddk_test_example");
    let _ = fs::create_dir_all(&dir);
    let dproj_path = dir.join("example.test.external.dproj");

    let xml = r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
    <PropertyGroup>
        <ProjectGuid>{9CE0D30C-9B83-4692-B96E-65128AE0E50F}</ProjectGuid>
        <MainSource>example.test.external.DPR</MainSource>
        <Configuration Condition=" '$(Configuration)' == '' ">Debug</Configuration>
        <DCC_DCCCompiler>DCC32</DCC_DCCCompiler>
        <DCC_DependencyCheckOutputName>..\build\bin\D12\example.test.external.exe</DCC_DependencyCheckOutputName>
        <FrameworkType>VCL</FrameworkType>
        <ProjectVersion>20.3</ProjectVersion>
        <Base>True</Base>
        <Config Condition="'$(Config)'==''">Debug</Config>
        <Platform Condition="'$(Platform)'==''">Win32</Platform>
        <TargetedPlatforms>1</TargetedPlatforms>
        <AppType>Application</AppType>
        <ProjectName Condition="'$(ProjectName)'==''">example.test.external</ProjectName>
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
        <DCC_ExeOutput>..\build\bin\D12</DCC_ExeOutput>
        <DCC_DcuOutput>..\build\dcu\D12</DCC_DcuOutput>
    </PropertyGroup>
    <ProjectExtensions>
        <Borland.Personality>Delphi.Personality.12</Borland.Personality>
        <Borland.ProjectType>VCLApplication</Borland.ProjectType>
        <BorlandProject>
            <Delphi.Personality>
                <Source>
                    <Source Name="MainSource">example.test.external.DPR</Source>
                </Source>
            </Delphi.Personality>
            <Platforms>
                <Platform value="Win32">True</Platform>
            </Platforms>
        </BorlandProject>
    </ProjectExtensions>
    <ItemGroup>
        <BuildConfiguration Include="Base">
            <Key>Base</Key>
        </BuildConfiguration>
        <BuildConfiguration Include="Debug">
            <Key>Cfg_2</Key>
            <CfgParent>Base</CfgParent>
        </BuildConfiguration>
    </ItemGroup>
</Project>"#;

    fs::write(&dproj_path, xml).unwrap();

    let dproj = Dproj::from_file(&dproj_path).unwrap();

    println!("=== dproj-rs results ===");
    println!("get_exe_path: {:?}", dproj.get_exe_path());
    println!("get_exe_path_for Debug/Win32: {:?}", dproj.get_exe_path_for("Debug", "Win32"));
    println!("active_configuration: {:?}", dproj.active_configuration());
    println!("active_platform: {:?}", dproj.active_platform());
}
