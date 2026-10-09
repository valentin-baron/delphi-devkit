use ddk_core::projects::*;

fn make_link(id: usize, project_id: usize) -> ProjectLink {
    ProjectLink { id, project_id }
}

fn make_workspace_with_links(links: Vec<ProjectLink>) -> Workspace {
    Workspace {
        id: 1,
        name: "TestWS".to_string(),
        compiler_id: "12.0".to_string(),
        project_links: links,
        ..Default::default()
    }
}

#[test]
fn new_project_link_on_empty_container() {
    let mut ws = make_workspace_with_links(vec![]);
    ws.new_project_link(100, 50);
    assert_eq!(ws.project_links.len(), 1);
    assert_eq!(ws.project_links[0].id, 100);
    assert_eq!(ws.project_links[0].project_id, 50);
}

#[test]
fn new_project_link_appends_at_end() {
    let mut ws = make_workspace_with_links(vec![
        make_link(1, 10),
    ]);
    ws.new_project_link(2, 20);
    assert_eq!(ws.project_links.len(), 2);
    assert_eq!(ws.project_links[0].id, 1);
    assert_eq!(ws.project_links[1].id, 2);
}

#[test]
fn index_of_found() {
    let ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(20, 2),
    ]);
    assert_eq!(ws.index_of(10), Some(0));
    assert_eq!(ws.index_of(20), Some(1));
}

#[test]
fn index_of_not_found() {
    let ws = make_workspace_with_links(vec![]);
    assert_eq!(ws.index_of(999), None);
}

#[test]
fn export_removes_link() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(20, 2),
    ]);
    let exported = ws.export_project_link(10).unwrap();
    assert_eq!(exported.id, 10);
    assert_eq!(ws.project_links.len(), 1);
    assert_eq!(ws.project_links[0].id, 20);
}

#[test]
fn export_nonexistent_link_fails() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
    ]);
    assert!(ws.export_project_link(999).is_err());
}

#[test]
fn import_at_end() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
    ]);
    let new_link = make_link(20, 2);
    ws.import_project_link(new_link, None).unwrap();
    assert_eq!(ws.project_links.len(), 2);
    assert_eq!(ws.project_links[1].id, 20);
}

#[test]
fn import_at_position() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(30, 3),
    ]);
    let new_link = make_link(20, 2);
    // Some(30) means: insert at the index link 30 occupies.
    ws.import_project_link(new_link, Some(30)).unwrap();
    assert_eq!(ws.project_links.len(), 3);
    assert_eq!(ws.project_links[1].id, 20);
}

#[test]
fn import_preserves_vec_order() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(20, 2),
    ]);
    let new_link = make_link(30, 3);
    ws.import_project_link(new_link, None).unwrap();
    let ids: Vec<usize> = ws.project_links.iter().map(|l| l.id).collect();
    assert_eq!(ids, vec![10, 20, 30]);
}

#[test]
fn move_link_to_different_position() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(20, 2),
        make_link(30, 3),
    ]);
    ws.move_project_link(10, Some(30)).unwrap();
    // Removing 10 gives [20,30]; inserting it at index_of(30)=1 -> [20,10,30].
    let ids: Vec<usize> = ws.project_links.iter().map(|l| l.id).collect();
    assert_eq!(ids, vec![20, 10, 30]);
}

#[test]
fn move_link_to_end() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
        make_link(20, 2),
    ]);
    ws.move_project_link(10, None).unwrap();
    assert_eq!(ws.project_links.last().unwrap().id, 10);
}

#[test]
fn move_nonexistent_link_fails() {
    let mut ws = make_workspace_with_links(vec![
        make_link(10, 1),
    ]);
    assert!(ws.move_project_link(999, None).is_err());
}

// GroupProject implements ProjectLinkContainer too.
#[test]
fn group_project_new_project_link() {
    let mut gp = GroupProject {
        name: "Test".to_string(),
        path: "test.groupproj".to_string(),
        project_links: vec![],
        ..Default::default()
    };
    gp.new_project_link(1, 100);
    assert_eq!(gp.project_links.len(), 1);
    assert_eq!(gp.project_links[0].project_id, 100);
}
