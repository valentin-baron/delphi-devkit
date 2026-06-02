use ddk_core::commands::*;
use ddk_core::projects::*;
use ddk_core::lexorank::LexoRank;

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
                    sort_rank: LexoRank::default(),
                },
            ],
            sort_rank: LexoRank::default(),
            ..Default::default()
        }],
        group_project: Some(GroupProject {
            name: "GP".into(),
            path: "gp.groupproj".into(),
            project_links: vec![
                ProjectLink {
                    id: 6,
                    project_id: 2,
                    sort_rank: LexoRank::default(),
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
        sort_rank: LexoRank::default(),
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

#[test]
fn compile_output_display_success() {
    let output = CompileOutput {
        project_name: "MyProject".into(),
        success: true,
        cancelled: false,
        code: 0,
        lines: vec![],
    };
    let display = format!("{}", output);
    assert!(display.contains("compiled successfully"));
}

#[test]
fn compile_output_display_failure() {
    let output = CompileOutput {
        project_name: "MyProject".into(),
        success: false,
        cancelled: false,
        code: 1,
        lines: vec!["error line".into()],
    };
    let display = format!("{}", output);
    assert!(display.contains("finished with errors"));
    assert!(display.contains("exit code 1"));
    assert!(display.contains("error line"));
}

#[test]
fn compile_output_display_cancelled() {
    let output = CompileOutput {
        project_name: "MyProject".into(),
        success: false,
        cancelled: true,
        code: -1,
        lines: vec![],
    };
    let display = format!("{}", output);
    assert!(display.contains("cancelled"));
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
