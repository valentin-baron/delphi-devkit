use ddk_core::projects::*;
use ddk_core::lexorank::LexoRank;

// ═══════════════════════════════════════════════════════════════════════════════
//  Helper: build a minimal ProjectsData for testing
// ═══════════════════════════════════════════════════════════════════════════════

fn make_project(id: usize, name: &str) -> Project {
    Project {
        id,
        name: name.to_string(),
        directory: format!("C:\\Projects\\{}", name),
        dproj: Some(format!("C:\\Projects\\{}\\{}.dproj", name, name)),
        ..Default::default()
    }
}

fn make_link(id: usize, project_id: usize, rank: &str) -> ProjectLink {
    ProjectLink {
        id,
        project_id,
        sort_rank: LexoRank::from_string(rank).unwrap_or_default(),
    }
}

fn make_workspace(id: usize, name: &str, links: Vec<ProjectLink>, rank: &str) -> Workspace {
    Workspace {
        id,
        name: name.to_string(),
        compiler_id: "12.0".to_string(),
        project_links: links,
        sort_rank: LexoRank::from_string(rank).unwrap_or_default(),
        ..Default::default()
    }
}

fn sample_data() -> ProjectsData {
    ProjectsData {
        id_counter: 10,
        active_project_id: Some(1),
        projects: vec![
            make_project(1, "Alpha"),
            make_project(2, "Beta"),
            make_project(3, "Gamma"),
        ],
        workspaces: vec![
            make_workspace(4, "WS-A", vec![
                make_link(5, 1, "1|d"),
                make_link(6, 2, "1|h"),
            ], "1|a"),
            make_workspace(7, "WS-B", vec![
                make_link(8, 3, "1|h"),
            ], "1|h"),
        ],
        group_project: None,
        group_project_compiler_id: "12.0".to_string(),
        ..Default::default()
    }
}

fn sample_data_with_group() -> ProjectsData {
    let mut data = sample_data();
    data.group_project = Some(GroupProject {
        name: "MyGroup".to_string(),
        path: "C:\\Groups\\MyGroup.groupproj".to_string(),
        project_links: vec![
            make_link(9, 1, "1|d"),
            make_link(10, 2, "1|h"),
        ],
        ..Default::default()
    });
    data
}

// ═══════════════════════════════════════════════════════════════════════════════
//  next_id
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn next_id_increments() {
    let mut data = ProjectsData::default();
    assert_eq!(data.next_id(), 1);
    assert_eq!(data.next_id(), 2);
    assert_eq!(data.next_id(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  get_project / get_workspace
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn get_project_found() {
    let data = sample_data();
    let project = data.get_project(1);
    assert!(project.is_some());
    assert_eq!(project.unwrap().name, "Alpha");
}

#[test]
fn get_project_not_found() {
    let data = sample_data();
    assert!(data.get_project(999).is_none());
}

#[test]
fn get_workspace_found() {
    let data = sample_data();
    let ws = data.get_workspace(4);
    assert!(ws.is_some());
    assert_eq!(ws.unwrap().name, "WS-A");
}

#[test]
fn get_workspace_not_found() {
    let data = sample_data();
    assert!(data.get_workspace(999).is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  can_find_any_links
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn can_find_links_in_workspace() {
    let data = sample_data();
    assert!(data.can_find_any_links(1)); // project 1 is linked in WS-A
}

#[test]
fn can_find_links_in_group_project() {
    let data = sample_data_with_group();
    assert!(data.can_find_any_links(1)); // also in group project
}

#[test]
fn cannot_find_links_for_missing_project() {
    let data = sample_data();
    assert!(!data.can_find_any_links(999));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  select_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn select_existing_project() {
    let mut data = sample_data();
    assert!(data.select_project(2).is_ok());
    assert_eq!(data.active_project_id, Some(2));
}

#[test]
fn select_nonexistent_project_fails() {
    let mut data = sample_data();
    assert!(data.select_project(999).is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  remove_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn remove_project_clears_active() {
    let mut data = sample_data();
    assert_eq!(data.active_project_id, Some(1));
    data.remove_project(1, true);
    assert_eq!(data.active_project_id, None);
}

#[test]
fn remove_project_with_links_removes_links() {
    let mut data = sample_data();
    data.remove_project(1, true);
    // No links to project 1 should remain
    for ws in &data.workspaces {
        for link in &ws.project_links {
            assert_ne!(link.project_id, 1);
        }
    }
}

#[test]
fn remove_project_without_links_keeps_links() {
    let mut data = sample_data();
    data.remove_project(1, false);
    // Links still reference project 1 (orphaned)
    let has_link = data.workspaces.iter()
        .flat_map(|ws| &ws.project_links)
        .any(|link| link.project_id == 1);
    assert!(has_link);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  remove_project_link
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn remove_last_link_removes_project() {
    let mut data = sample_data();
    // Link 8 is the only link to project 3
    data.remove_project_link(8);
    assert!(data.get_project(3).is_none());
}

#[test]
fn remove_non_last_link_keeps_project() {
    let mut data = sample_data_with_group();
    // Project 1 has link 5 (WS-A) and link 9 (group project)
    data.remove_project_link(5);
    assert!(data.get_project(1).is_some());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  remove_workspace
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn remove_workspace_orphans_unique_projects() {
    let mut data = sample_data();
    // WS-B has project 3, which is only in WS-B
    data.remove_workspace(7);
    assert!(data.get_project(3).is_none());
    assert!(data.get_workspace(7).is_none());
}

#[test]
fn remove_workspace_keeps_shared_projects() {
    let mut data = sample_data();
    // Project 1 is in WS-A (link 5). Remove WS-A.
    // Project 1 has no other links, so it gets removed too.
    // But project 2 is also only in WS-A, so it also gets removed.
    let initial_count = data.projects.len();
    data.remove_workspace(4);
    // WS-A had 2 unique projects (1 and 2)
    assert_eq!(data.projects.len(), initial_count - 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  remove_group_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn remove_group_project_clears_it() {
    let mut data = sample_data_with_group();
    data.remove_group_project();
    assert!(data.group_project.is_none());
}

#[test]
fn remove_group_project_keeps_workspace_linked_projects() {
    let mut data = sample_data_with_group();
    data.remove_group_project();
    // Projects 1 and 2 are still in WS-A, so they should be kept
    assert!(data.get_project(1).is_some());
    assert!(data.get_project(2).is_some());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  sort
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn sort_orders_workspaces_by_rank() {
    let mut data = ProjectsData {
        workspaces: vec![
            make_workspace(2, "Second", vec![], "1|h"),
            make_workspace(1, "First", vec![], "1|a"),
        ],
        ..Default::default()
    };
    data.sort();
    assert_eq!(data.workspaces[0].name, "First");
    assert_eq!(data.workspaces[1].name, "Second");
}

#[test]
fn sort_orders_links_within_workspace() {
    let mut data = ProjectsData {
        projects: vec![make_project(1, "A"), make_project(2, "B")],
        workspaces: vec![
            make_workspace(10, "WS", vec![
                make_link(12, 2, "1|h"),
                make_link(11, 1, "1|a"),
            ], "1|h"),
        ],
        ..Default::default()
    };
    data.sort();
    assert_eq!(data.workspaces[0].project_links[0].id, 11); // "1|a" first
    assert_eq!(data.workspaces[0].project_links[1].id, 12); // "1|h" second
}

// ═══════════════════════════════════════════════════════════════════════════════
//  find_project_by_dproj
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn find_project_by_dproj_found() {
    let data = sample_data();
    let result = data.find_project_by_dproj(&r"C:\Projects\Alpha\Alpha.dproj".to_string());
    assert!(result.is_some());
    assert_eq!(result.unwrap().name, "Alpha");
}

#[test]
fn find_project_by_dproj_not_found() {
    let data = sample_data();
    assert!(data.find_project_by_dproj(&"nonexistent.dproj".to_string()).is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  active_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn active_project_returns_correct_project() {
    let data = sample_data();
    let active = data.active_project();
    assert!(active.is_some());
    assert_eq!(active.unwrap().id, 1);
}

#[test]
fn active_project_none_when_no_selection() {
    let data = ProjectsData::default();
    assert!(data.active_project().is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  get_workspace_id_containing_project_link
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn finds_workspace_containing_link() {
    let data = sample_data();
    assert_eq!(data.get_workspace_id_containing_project_link(5), Some(4));
    assert_eq!(data.get_workspace_id_containing_project_link(8), Some(7));
}

#[test]
fn returns_none_for_unknown_link() {
    let data = sample_data();
    assert_eq!(data.get_workspace_id_containing_project_link(999), None);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  is_project_link_in_group_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn link_in_group_project() {
    let data = sample_data_with_group();
    assert!(data.is_project_link_in_group_project(9));
    assert!(data.is_project_link_in_group_project(10));
}

#[test]
fn link_not_in_group_project() {
    let data = sample_data_with_group();
    assert!(!data.is_project_link_in_group_project(5)); // in workspace
    assert!(!data.is_project_link_in_group_project(999));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  projects_of_workspace / projects_of_group_project
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn projects_of_workspace_returns_linked_projects() {
    let data = sample_data();
    let ws = data.get_workspace(4).unwrap();
    let projects = data.projects_of_workspace(ws);
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].name, "Alpha");
    assert_eq!(projects[1].name, "Beta");
}

#[test]
fn projects_of_group_project_returns_linked_projects() {
    let data = sample_data_with_group();
    let gp = data.group_project.as_ref().unwrap();
    let projects = data.projects_of_group_project(gp);
    assert_eq!(projects.len(), 2);
}
