use ddk_core::commands::*;
use ddk_core::projects::*;

// ═══════════════════════════════════════════════════════════════════════════════
//  find_project_link_id
// ═══════════════════════════════════════════════════════════════════════════════

fn make_data() -> ProjectsData {
    ProjectsData {
        id_counter: 10,
        active_project_id: Some(1),
        projects: vec![
            Project {
                id: 1,
                name: "Alpha".into(),
                directory: "dir".into(),
                ..Default::default()
            },
            Project {
                id: 2,
                name: "Beta".into(),
                directory: "dir".into(),
                ..Default::default()
            },
            Project {
                id: 3,
                name: "Gamma".into(),
                directory: "dir".into(),
                ..Default::default()
            },
        ],
        workspaces: vec![Workspace {
            id: 4,
            name: "WS".into(),
            compiler_id: "12.0".into(),
            project_links: vec![
                ProjectLink {
                    id: 5,
                    project_id: 1,
                },
            ],
            ..Default::default()
        }],
        group_project: Some(GroupProject {
            name: "GP".into(),
            path: "gp.groupproj".into(),
            project_links: vec![
                ProjectLink {
                    id: 6,
                    project_id: 2,
                },
            ],
            ..Default::default()
        }),
        group_project_compiler_id: "12.0".into(),
        ..Default::default()
    }
}

#[test]
fn find_link_in_workspace() {
    let data = make_data();
    assert_eq!(find_project_link_id(&data, 1), Some(5));
}

#[test]
fn find_link_in_group_project() {
    let data = make_data();
    assert_eq!(find_project_link_id(&data, 2), Some(6));
}

#[test]
fn find_link_not_found() {
    let data = make_data();
    assert_eq!(find_project_link_id(&data, 3), None); // project 3 has no links
}

#[test]
fn find_link_prefers_workspace_over_group() {
    // If a project is in both workspace and group project, workspace wins
    // (because workspaces are searched first).
    let mut data = make_data();
    // Add project 2 to workspace as well
    data.workspaces[0].project_links.push(ProjectLink {
        id: 7,
        project_id: 2,
    });
    assert_eq!(find_project_link_id(&data, 2), Some(7)); // workspace link, not 6
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Display – ProjectListResult
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn project_list_display_empty() {
    let result = ProjectListResult {
        workspaces: vec![],
        group_project: None,
        active_project_id: None,
    };
    let display = format!("{}", result);
    assert_eq!(display, "No projects found.");
}

#[test]
fn project_list_display_with_workspace() {
    let result = ProjectListResult {
        workspaces: vec![WorkspaceSummary {
            id: 1,
            name: "MyWS".into(),
            compiler_id: "12.0".into(),
            projects: vec![ProjectSummary {
                id: 10,
                name: "Proj".into(),
                directory: "dir".into(),
                dproj: None,
                exe: None,
                host: None,
                active: true,
            }],
        }],
        group_project: None,
        active_project_id: Some(10),
    };
    let display = format!("{}", result);
    assert!(display.contains("MyWS"));
    assert!(display.contains("12.0"));
    assert!(display.contains("*")); // active marker
    assert!(display.contains("Proj"));
}

#[test]
fn project_list_display_empty_workspace() {
    let result = ProjectListResult {
        workspaces: vec![WorkspaceSummary {
            id: 1,
            name: "EmptyWS".into(),
            compiler_id: "12.0".into(),
            projects: vec![],
        }],
        group_project: None,
        active_project_id: None,
    };
    let display = format!("{}", result);
    assert!(display.contains("(empty)"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Display – CompileOutput
// ═══════════════════════════════════════════════════════════════════════════════

fn sample_output(success: bool, code: i32) -> CompileOutput {
    CompileOutput {
        project: "MyProject".into(),
        project_path: r"C:\proj\MyProject.dproj".into(),
        compiler: "Delphi 12.0 Athens".into(),
        config: Some("Release".into()),
        platform: Some("Win32".into()),
        action: "compile".into(),
        success,
        code,
        diagnostics: Default::default(),
    }
}

#[test]
fn compile_output_display_success() {
    let output = sample_output(true, 0);
    let display = format!("{}", output);
    assert!(display.contains("compiled successfully"));
}

#[test]
fn compile_output_display_failure() {
    let mut output = sample_output(false, 1);
    output.diagnostics.errors.push(CompileDiagnostic {
        code: "E2003".into(),
        file: r"C:\proj\MyProject.dpr".into(),
        line: 4,
        message: "Undeclared identifier".into(),
    });
    let display = format!("{}", output);
    assert!(display.contains("finished with errors"));
    assert!(display.contains("exit code 1"));
    assert!(display.contains("1 errors"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Display – EnvironmentInfo
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn environment_info_display_no_project() {
    let info = EnvironmentInfo {
        project: None,
        group_project_compiler: None,
    };
    let display = format!("{}", info);
    assert!(display.contains("No active project"));
}

#[test]
fn environment_info_display_with_project() {
    let info = EnvironmentInfo {
        project: Some(EnvironmentProject {
            id: 1,
            name: "TestProj".into(),
            directory: r"C:\dir".into(),
            dproj: Some("test.dproj".into()),
            compilers: vec![EnvironmentCompilerEntry {
                context: "WS-A".into(),
                key: "12.0".into(),
                product_name: "Delphi 12".into(),
                product_version: 29,
                compiler_version: 36,
                installation_path: r"C:\Delphi".into(),
            }],
        }),
        group_project_compiler: None,
    };
    let display = format!("{}", info);
    assert!(display.contains("TestProj"));
    assert!(display.contains("Delphi 12"));
    assert!(display.contains("WS-A"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Display – other types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn select_project_result_display() {
    let result = SelectProjectResult {
        project_id: 42,
        project_name: "MyProj".into(),
    };
    let display = format!("{}", result);
    assert!(display.contains("MyProj"));
    assert!(display.contains("42"));
}

#[test]
fn compiler_summary_display() {
    let summary = CompilerSummary {
        key: "12.0".into(),
        product_name: "Delphi 12".into(),
        product_version: 29,
        compiler_version: 36,
        installation_path: r"C:\Delphi".into(),
    };
    let display = format!("{}", summary);
    assert!(display.contains("12.0"));
    assert!(display.contains("Delphi 12"));
}

#[test]
fn format_file_result_display() {
    let result = FormatFileResult {
        file_path: "test.pas".into(),
    };
    let display = format!("{}", result);
    assert!(display.contains("Formatted: test.pas"));
}

#[test]
fn set_compiler_result_display() {
    let result = SetCompilerResult {
        key: "12.0".into(),
        product_name: "Delphi 12".into(),
    };
    let display = format!("{}", result);
    assert!(display.contains("Delphi 12"));
    assert!(display.contains("12.0"));
}

#[test]
fn add_project_result_display() {
    let result = AddProjectResult {
        project_id: 7,
        project_name: "MyApp".into(),
        workspace_id: 3,
        workspace_name: "Workspace 1".into(),
        dproj: None,
        dpr: Some(r"C:\temp\MyApp.dpr".into()),
        dpk: None,
        exe: Some(r"C:\temp\MyApp.exe".into()),
    };
    let display = format!("{}", result);
    assert!(display.contains("MyApp"));
    assert!(display.contains("Workspace 1"));
    assert!(display.contains("7"));
}

#[test]
fn add_workspace_result_display() {
    let result = AddWorkspaceResult {
        workspace_id: 5,
        name: "Workspace 1".into(),
        compiler_key: "12.0".into(),
        compiler_product_name: "Delphi 12.0 Athens".into(),
    };
    let display = format!("{}", result);
    assert!(display.contains("Workspace 1"));
    assert!(display.contains("Delphi 12.0 Athens"));
    assert!(display.contains("12.0"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  resolve_workspace_id
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn resolve_workspace_by_exact_name() {
    let data = make_data();
    assert_eq!(resolve_workspace_id(&data, "WS").unwrap(), 4);
}

#[test]
fn resolve_workspace_case_insensitive() {
    let data = make_data();
    assert_eq!(resolve_workspace_id(&data, "ws").unwrap(), 4);
}

#[test]
fn resolve_workspace_by_numeric_id() {
    let data = make_data();
    assert_eq!(resolve_workspace_id(&data, "4").unwrap(), 4);
}

#[test]
fn resolve_workspace_not_found_errors() {
    let data = make_data();
    let err = resolve_workspace_id(&data, "Nope").unwrap_err();
    assert!(err.to_string().contains("not found"));
    // Should list the available workspace name.
    assert!(err.to_string().contains("WS"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  resolve_project_reference
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn resolve_project_by_numeric_id() {
    let data = make_data();
    match resolve_project_reference(&data, "2") {
        ProjectResolution::Single(id) => assert_eq!(id, 2),
        other => panic!("expected Single(2), got {other:?}"),
    }
}

#[test]
fn resolve_project_by_exact_name_case_insensitive() {
    let data = make_data();
    match resolve_project_reference(&data, "alpha") {
        ProjectResolution::Single(id) => assert_eq!(id, 1),
        other => panic!("expected Single(1), got {other:?}"),
    }
}

#[test]
fn resolve_project_substring_single() {
    let data = make_data();
    // "amm" only occurs in "Gamma".
    match resolve_project_reference(&data, "amm") {
        ProjectResolution::Single(id) => assert_eq!(id, 3),
        other => panic!("expected Single(3), got {other:?}"),
    }
}

#[test]
fn resolve_project_ambiguous_lists_candidates() {
    let mut data = make_data();
    // Two distinct projects sharing the name "be".
    data.projects.push(Project { id: 20, name: "be".into(), directory: "d1".into(), ..Default::default() });
    data.projects.push(Project { id: 21, name: "BE".into(), directory: "d2".into(), ..Default::default() });
    match resolve_project_reference(&data, "be") {
        ProjectResolution::Ambiguous(matches) => {
            let ids: Vec<usize> = matches.iter().map(|m| m.id).collect();
            assert!(ids.contains(&20) && ids.contains(&21));
            assert_eq!(matches.len(), 2);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn resolve_project_not_found() {
    let data = make_data();
    matches!(resolve_project_reference(&data, "zzz"), ProjectResolution::NotFound);
}

#[test]
fn resolve_project_by_path_unique() {
    let mut data = make_data();
    data.projects[0].dproj = Some(r"C:\src\be\D12\be.dproj".into());
    match resolve_project_by_path(&data, r"C:\src\be\D12\be.dproj") {
        ProjectResolution::Single(id) => assert_eq!(id, 1),
        other => panic!("expected Single(1), got {other:?}"),
    }
}

#[test]
fn resolve_project_by_path_case_insensitive_and_separators() {
    let mut data = make_data();
    data.projects[1].dpr = Some(r"C:\src\be\D12\be.dpr".into());
    // Different case + forward slashes must still match.
    match resolve_project_by_path(&data, r"c:/SRC/be/D12/BE.dpr") {
        ProjectResolution::Single(id) => assert_eq!(id, 2),
        other => panic!("expected Single(2), got {other:?}"),
    }
}

#[test]
fn resolve_project_by_path_ambiguous() {
    let mut data = make_data();
    // Two projects referencing the same .dproj file.
    data.projects[0].dproj = Some(r"C:\shared\thing.dproj".into());
    data.projects[1].dproj = Some(r"C:\shared\thing.dproj".into());
    match resolve_project_by_path(&data, r"C:\shared\thing.dproj") {
        ProjectResolution::Ambiguous(matches) => {
            let ids: Vec<usize> = matches.iter().map(|m| m.id).collect();
            assert!(ids.contains(&1) && ids.contains(&2));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn resolve_project_by_path_not_found_falls_through() {
    let data = make_data();
    matches!(
        resolve_project_by_path(&data, r"C:\nowhere\unmanaged.dpr"),
        ProjectResolution::NotFound
    );
}

#[test]
fn ambiguous_projects_display_matches_spec() {
    let amb = AmbiguousProjects {
        reference: "be".into(),
        matches: vec![
            ProjectRef { id: 123, name: "be".into(), location: "Workspace 1".into(), path: r"path\to\be.dpr".into() },
            ProjectRef { id: 124, name: "be".into(), location: "Workspace 2".into(), path: r"other\be.dpr".into() },
        ],
    };
    let display = format!("{amb}");
    assert!(display.contains("Project \"be\" matches multiple projects:"));
    assert!(display.contains("- ID 123 = Workspace 1 - be (path\\to\\be.dpr)"));
    assert!(display.contains("- ID 124 = Workspace 2 - be"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  DelphiLspConfigResult
// ═══════════════════════════════════════════════════════════════════════════════

fn delphilsp_result() -> ddk_core::delphilsp::DelphiLspConfigResult {
    ddk_core::delphilsp::DelphiLspConfigResult {
        file_path: r"C:\proj\App.delphilsp.json".into(),
        project_file: r"C:\proj\App.dproj".into(),
        project_uri: "file:///C%3A/proj/App.dpr".into(),
        dllname: "dcc64290.dll".into(),
        configuration: "Debug".into(),
        platform: "Win64".into(),
        compiler: "Delphi 12.0 Athens".into(),
        search_path_count: 42,
        browsing_path_count: 7,
        define_count: 2,
        warnings: Vec::new(),
    }
}

#[test]
fn delphilsp_result_display_summarises_the_written_file() {
    let display = format!("{}", delphilsp_result());
    assert!(display.contains(r"Wrote C:\proj\App.delphilsp.json"), "{display}");
    assert!(display.contains("Debug / Win64"), "{display}");
    assert!(display.contains("Delphi 12.0 Athens (dcc64290.dll)"), "{display}");
    assert!(display.contains("42 entries, 7 browsing paths, 2 defines"), "{display}");
}

#[test]
fn delphilsp_result_display_lists_warnings() {
    let mut result = delphilsp_result();
    result.warnings.push("Dropped search-path entry with unresolved macro: $(NOPE)".into());
    let display = format!("{result}");
    assert!(display.contains("! Dropped search-path entry with unresolved macro: $(NOPE)"), "{display}");
}

#[test]
fn delphilsp_result_serialises_every_field() {
    let json = serde_json::to_value(delphilsp_result()).unwrap();
    for key in [
        "file_path", "project_file", "project_uri", "dllname", "configuration",
        "platform", "compiler", "search_path_count", "browsing_path_count",
        "define_count", "warnings",
    ] {
        assert!(json.get(key).is_some(), "missing {key} in {json}");
    }
}
