use crate::state::{PROJECTS_DATA, PROJECTS_DATA_CHANGED, Stateful};
use crate::utils::{FilePath, Load, normalize_path};
use std::sync::Arc;
use tokio::sync::RwLock;
use super::*;
use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use std::collections::{HashSet, HashMap};
use std::sync::atomic::AtomicBool;

enum IdObject {
    Workspace,
    Project,
    ProjectLink,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ProjectsData {
    pub id_counter: usize,
    pub active_project_id: Option<usize>,
    pub workspaces: Vec<Workspace>,
    pub projects: Vec<Project>,
    pub group_project: Option<GroupProject>,
    pub group_project_compiler_id: String,
}

impl Stateful for ProjectsData {
    fn internal_change_flag() -> &'static AtomicBool {
        &PROJECTS_DATA_CHANGED
    }
    fn get_state() -> &'static Arc<RwLock<Self>> {
        &PROJECTS_DATA
    }
}

impl Default for ProjectsData {
    fn default() -> Self {
        ProjectsData {
            id_counter: 0,
            active_project_id: None,
            workspaces: Vec::new(),
            projects: Vec::new(),
            group_project: None,
            group_project_compiler_id: String::from("12.0"),
        }
    }
}

impl ProjectsData {
    pub fn new() -> Self {
        return Self::load_from_file(&Self::get_file_path());
    }

    pub async fn group_projects_compiler(&self) -> CompilerConfiguration {
        let compilers = COMPILER_CONFIGURATIONS.read().await;
        if let Some(compiler) = compilers.get(&self.group_project_compiler_id.to_string()) {
            return compiler.clone();
        }
        return compilers
            .get("12.0")
            .expect(format!(
                "Compiler with id {} not found; should not be possible.",
                self.group_project_compiler_id).as_str())
            .clone();
    }

    async fn validate_compilers(&self) -> Result<()> {
        for workspace in &self.workspaces {
            if !compiler_exists(&workspace.compiler_id).await {
                anyhow::bail!("Workspace '{}' has invalid compiler id: {}", workspace.name, workspace.compiler_id);
            }
        }
        if !compiler_exists(&self.group_project_compiler_id).await {
            anyhow::bail!("Group project compiler has invalid id: {}", self.group_project_compiler_id);
        }
        Ok(())
    }

    fn get_id_map(&self) -> Result<HashMap<usize, IdObject>> {
        let mut id_map = HashMap::new();
        for workspace in &self.workspaces {
            if id_map.contains_key(&workspace.id) {
                anyhow::bail!("Duplicate id found: {}", workspace.id);
            }
            id_map.insert(workspace.id, IdObject::Workspace);
            for project_link in &workspace.project_links {
                if id_map.contains_key(&project_link.id) {
                    anyhow::bail!("Duplicate id found: {}", project_link.id);
                }
                id_map.insert(project_link.id, IdObject::ProjectLink);
            }
        }
        if let Some(group_project) = &self.group_project {
            for project_link in &group_project.project_links {
                if id_map.contains_key(&project_link.id) {
                    anyhow::bail!("Duplicate id found: {}", project_link.id);
                }
                id_map.insert(project_link.id, IdObject::ProjectLink);
            }
        }
        for project in &self.projects {
            if id_map.contains_key(&project.id) {
                anyhow::bail!("Duplicate id found: {}", project.id);
            }
            id_map.insert(project.id, IdObject::Project);
        }
        return Ok(id_map)
    }

    fn validate_project_references(&self, id_map: &HashMap<usize, IdObject>) -> Result<()> {
        if let Some(active_id) = self.active_project_id {
            match id_map.get(&active_id) {
                Some(IdObject::Project) => {},
                _ => anyhow::bail!("Active project id {} does not refer to a valid project", active_id),
            }
        }
        if let Some(group_project) = &self.group_project {
            for project_link in &group_project.project_links {
                match id_map.get(&project_link.project_id) {
                    Some(IdObject::Project) => {},
                    _ => anyhow::bail!("Group project link id {} refers to invalid project id {}", project_link.id, project_link.project_id),
                }
            }
        }
        for workspace in &self.workspaces {
            for project_link in &workspace.project_links {
                match id_map.get(&project_link.project_id) {
                    Some(IdObject::Project) => {},
                    _ => anyhow::bail!("Workspace '{}' link id {} refers to invalid project id {}", workspace.name, project_link.id, project_link.project_id),
                }
            }
        }
        Ok(())
    }

    pub async fn validate(&self) -> Result<()> {
        self.validate_compilers().await?;
        let id_map = self.get_id_map()?;
        self.validate_project_references(&id_map)?;
        let mut workspace_names: HashSet<&String> = HashSet::new();
        for workspace in &self.workspaces {
            if workspace_names.contains(&workspace.name) {
                anyhow::bail!("Duplicate workspace name found: {}", workspace.name);
            }
            if workspace.name.trim().is_empty() {
                anyhow::bail!("Workspace name cannot be empty: {}", workspace.id);
            }
            workspace_names.insert(&workspace.name);
        }
        Ok(())
    }

    pub fn next_id(&mut self) -> usize {
        self.id_counter += 1;
        return self.id_counter;
    }

    pub fn can_find_any_links(&self, project_id: usize) -> bool {
        for workspace in &self.workspaces {
            for project_link in &workspace.project_links {
                if project_link.project_id == project_id {
                    return true;
                }
            }
        }
        if let Some(group_project) = &self.group_project {
            for project_link in &group_project.project_links {
                if project_link.project_id == project_id {
                    return true;
                }
            }
        }
        return false;
    }

    pub fn new_project(&mut self, file_path: &String, workspace_id: usize) -> Result<()> {
        let (project_id, link_id) = (self.id_counter + 1, self.id_counter + 2);
        let workspace = match self.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
            Some(ws) => ws,
            _ => anyhow::bail!("Workspace with id {} not found", workspace_id),
        };
        let file = PathBuf::from(file_path);
        let file = normalize_path(&file);
        let file_path = file.to_string_lossy().to_string();
        let project = match file.extension().and_then(|ext| ext.to_str()).map(|s| s.to_lowercase()) {
            Some(ext) if ext == "dproj" => {
                Project {
                    id: project_id,
                    name: file.file_stem().and_then(|s| s.to_str()).unwrap_or("<name error>").to_string(),
                    directory: file.parent().and_then(|p| p.to_str()).unwrap_or("<directory error>").to_string(),
                    dproj: Some(file_path.clone()),
                    ..Default::default()
                }
            },
            Some(ext) if ext == "dpr" => {
                Project {
                    id: project_id,
                    name: file.file_stem().and_then(|s| s.to_str()).unwrap_or("<name error>").to_string(),
                    directory: file.parent().and_then(|p| p.to_str()).unwrap_or("<directory error>").to_string(),
                    dpr: Some(file_path.clone()),
                    ..Default::default()
                }
            },
            Some(ext) if ext == "dpk" => {
                Project {
                    id: project_id,
                    name: file.file_stem().and_then(|s| s.to_str()).unwrap_or("<name error>").to_string(),
                    directory: file.parent().and_then(|p| p.to_str()).unwrap_or("<directory error>").to_string(),
                    dpk: Some(file_path.clone()),
                    ..Default::default()
                }
            },
            _ => {
                anyhow::bail!("Unsupported project file type: {}", file_path);
            }
        };
        workspace.project_links.push(ProjectLink {
            id: link_id,
            project_id: project.id,
            sort_rank: LexoRank::default(),
        });
        self.projects.push(project);
        self.next_id(); // for project_id
        self.next_id(); // for link_id

        // Discover exe/ini paths from the dproj right away so they are
        // populated even when the executable hasn't been built yet.
        if let Some(proj) = self.projects.last_mut() {
            let _ = proj.discover_paths();
        }

        return Ok(());
    }


    pub fn add_project_link(&mut self, project_id: usize, workspace_id: usize) -> Result<()> {
        if self.get_project(project_id).is_none() {
            anyhow::bail!("Project with id {} not found", project_id);
        }
        let id = self.id_counter + 1;
        let workspace = match self.get_workspace_mut(workspace_id) {
            Some(ws) => ws,
            _ => anyhow::bail!("Workspace with id {} not found", workspace_id),
        };
        workspace.project_links.push(ProjectLink {
            id,
            project_id,
            sort_rank: LexoRank::default(),
        });
        self.next_id();
        return Ok(());
    }

    pub fn remove_project(&mut self, project_id: usize, remove_links: bool) {
        self.projects.retain(|proj| proj.id != project_id);

        if Some(project_id) == self.active_project_id {
            self.active_project_id = None;
        }

        if remove_links {
            for workspace in &mut self.workspaces {
                workspace.project_links.retain(|link| link.project_id != project_id);
            }
            if let Some(group_project) = &mut self.group_project {
                group_project.project_links.retain(|link| link.project_id != project_id);
            }
        }
    }

    pub fn remove_project_link(&mut self, project_link_id: usize) {
        let mut project_id: Option<usize> = None;
        for workspace in &mut self.workspaces {
            if let Some(pos) = workspace.project_links.iter().position(|link| link.id == project_link_id) {
                project_id = Some(workspace.project_links[pos].project_id);
                workspace.project_links.remove(pos);
                break;
            }
        }
        if project_id.is_none() {
            if let Some(group_project) = &mut self.group_project &&
               let Some(pos) = group_project.project_links.iter().position(|link| link.id == project_link_id) {
                project_id = Some(group_project.project_links[pos].project_id);
                group_project.project_links.remove(pos);
            }
        }
        if project_id.is_some() && !self.can_find_any_links(project_id.unwrap()) {
            self.remove_project(project_id.unwrap(), false);
        }
    }

    pub fn move_project_link(&mut self, project_link_id: usize, drop_target: usize) -> Result<()> {
        let id_map = self.get_id_map()?;
        if !id_map.contains_key(&drop_target) {
            anyhow::bail!("Drop target id {} not found", drop_target);
        }
        match id_map.get(&project_link_id) {
            Some(IdObject::ProjectLink) => {},
            _ => anyhow::bail!("Project link with id {} not found", project_link_id),
        };
        let target_link_id: Option<usize> = id_map.get(&drop_target).map(|obj| match obj {
            IdObject::ProjectLink => Some(drop_target),
            _ => None,
        }).flatten();
        let source_workspace_id = self.get_workspace_id_containing_project_link(project_link_id);
        let target_workspace_id = match id_map.get(&drop_target) {
            Some(IdObject::Workspace) => Some(drop_target),
            Some(IdObject::ProjectLink) => self.get_workspace_id_containing_project_link(drop_target),
            _ => anyhow::bail!("Invalid drop target with id {}.", drop_target),
        };
        drop(id_map);
        if let Some(source_workspace_id) = source_workspace_id {
            if let Some(target_workspace_id) = target_workspace_id {
                let source_workspace = self.get_workspace_mut(source_workspace_id)
                    .ok_or_else(|| anyhow::anyhow!("Unable to find source workspace"))?;
                if target_workspace_id == source_workspace_id {
                    return source_workspace.move_project_link(project_link_id, target_link_id);
                } else {
                    let link = source_workspace.export_project_link(project_link_id)?;
                    let target_workspace = self.get_workspace_mut(target_workspace_id)
                        .ok_or_else(|| anyhow::anyhow!("Unable to find target workspace"))?;
                    target_workspace.import_project_link(link, target_link_id)?;
                    return Ok(());
                }
            } else {
                anyhow::bail!("Cannot move project link from workspace to group project.");
            }
        } else if let Some(_) = target_workspace_id {
            anyhow::bail!("Cannot move project link from group project to workspace.");
        } else {
            todo!("Move within group project on top of that element");
        }
    }

    pub fn get_workspace_id_containing_project_link(&self, project_link_id: usize) -> Option<usize> {
        for workspace in &self.workspaces {
            if workspace.project_links.iter().any(|link| link.id == project_link_id) {
                return Some(workspace.id);
            }
        }
        return None;
    }

    pub fn is_project_link_in_group_project(&self, project_link_id: usize) -> bool {
        if let Some(group_project) = &self.group_project {
            if group_project.project_links.iter().any(|link| link.id == project_link_id) {
                return true;
            }
        }
        return false;
    }

    pub fn refresh_project_paths(&mut self, project_id: usize) -> Result<()> {
        let project = match self.get_project_mut(project_id) {
            Some(proj) => proj,
            _ => anyhow::bail!("Project with id {} not found", project_id),
        };
        return project.discover_paths();
    }

    pub fn update_project(&mut self, project_id: usize, data: ProjectUpdateData) -> Result<()> {
        let project = match self.get_project_mut(project_id) {
            Some(proj) => proj,
            _ => anyhow::bail!("Project with id {} not found", project_id),
        };
        if let Some(name) = data.name {
            project.name = name;
        }
        if let Some(directory) = data.directory {
            let directory = normalize_path(&directory).to_string_lossy().to_string();
            if !PathBuf::from(&directory).exists() {
                anyhow::bail!("Directory does not exist: {}", directory);
            }
            project.directory = directory;
        }
        if let Some(dproj) = data.dproj {
            let dproj = normalize_path(&dproj).to_string_lossy().to_string();
            if !PathBuf::from(&dproj).exists() {
                anyhow::bail!(".dproj file does not exist: {}", dproj);
            }
            project.dproj = Some(dproj);
        }
        if let Some(dpr) = data.dpr {
            let dpr = normalize_path(&dpr).to_string_lossy().to_string();
            if !PathBuf::from(&dpr).exists() {
                anyhow::bail!(".dpr file does not exist: {}", dpr);
            }
            project.dpr = Some(dpr);
        }
        if let Some(dpk) = data.dpk {
            let dpk = normalize_path(&dpk).to_string_lossy().to_string();
            if !PathBuf::from(&dpk).exists() {
                anyhow::bail!(".dpk file does not exist: {}", dpk);
            }
            project.dpk = Some(dpk);
        }
        if let Some(exe) = data.exe {
            let exe = normalize_path(&exe).to_string_lossy().to_string();
            if !PathBuf::from(&exe).exists() {
                anyhow::bail!(".exe file does not exist: {}", exe);
            }
            project.exe = Some(exe);
        }
        if let Some(ini) = data.ini {
            let ini = normalize_path(&ini).to_string_lossy().to_string();
            if !PathBuf::from(&ini).exists() {
                anyhow::bail!(".ini file does not exist: {}", ini);
            }
            project.ini = Some(ini);
        }
        if let Some(start_parameters) = data.start_parameters {
            let start_parameters = start_parameters.trim().to_string();
            project.start_parameters = if start_parameters.is_empty() { None } else { Some(start_parameters) };
        }
        if let Some(host_application) = data.host_application {
            let host_application = host_application.trim().to_string();
            project.host_application = if host_application.is_empty() { None } else { Some(host_application) };
        }
        return Ok(());
    }

    pub fn select_project(&mut self, project_id: usize) -> Result<()> {
        let project = match self.get_project(project_id) {
            Some(proj) => proj,
            _ => anyhow::bail!("Project with id {} not found", project_id),
        };
        self.active_project_id = Some(project.id);
        return Ok(());
    }

    pub async fn new_workspace(&mut self, name: &String, compiler: &String) -> Result<()> {
        if !compiler_exists(compiler).await {
           anyhow::bail!("Compiler not found: {}", compiler);
        }
        let workspace_id = self.next_id();
        let lexo_rank = if let Some(last_ws) = self.workspaces.last() {
            &last_ws.sort_rank
        } else {
            &LexoRank::default()
        };
        let workspace = Workspace::new(workspace_id, name.clone(), compiler.clone(), lexo_rank.next());
        self.workspaces.push(workspace);
        return Ok(());
    }

    pub fn remove_workspace(&mut self, workspace_id: usize) {
        let project_ids: Vec<usize> = self.workspaces
            .iter()
            .find(|ws| ws.id == workspace_id)
            .map(|ws| ws.project_links.iter().map(|link| link.project_id).collect())
            .unwrap_or_default();

        self.workspaces.retain(|ws| ws.id != workspace_id);
        for project_id in project_ids {
            if !self.can_find_any_links(project_id) {
                self.remove_project(project_id, false);
            }
        }
    }

    pub fn move_workspace(&mut self, workspace_id: usize, drop_target_id: usize) -> Result<()> {
        let id_map = self.get_id_map()?;
        match id_map.get(&drop_target_id).ok_or_else(|| anyhow::anyhow!("Drop target id {} not found", drop_target_id))? {
            IdObject::Workspace => {
                let workspace_index = self.get_workspace_index(workspace_id)
                    .ok_or_else(|| anyhow::anyhow!("Unable to find workspace to move in list"))?;
                let drop_index = self.get_workspace_index(drop_target_id);
                let workspace = self.workspaces.remove(workspace_index);
                if let Some(drop_index) = drop_index {
                    self.workspaces.insert(drop_index, workspace);
                } else {
                    self.workspaces.push(workspace);
                }
            },
            IdObject::ProjectLink => {
                let workspace_index = self.get_workspace_index(workspace_id)
                    .ok_or_else(|| anyhow::anyhow!("Unable to find workspace to move in list"))?;
                let containing_workspace_id = self.get_workspace_id_containing_project_link(drop_target_id);
                let drop_index = if let Some(id) = containing_workspace_id {
                    self.get_workspace_index(id)
                } else {
                    None
                };
                let workspace = self.workspaces.remove(workspace_index);
                if let Some(drop_index) = drop_index {
                    self.workspaces.insert(drop_index, workspace);
                } else {
                    self.workspaces.push(workspace);
                }

            }
            _ => anyhow::bail!("Invalid drop target with id {}.", drop_target_id),
        }
        let mut workspaces: Vec<&mut dyn HasLexoRank> = self.workspaces.iter_mut().map(|ws| ws as &mut dyn HasLexoRank).collect();
        LexoRank::apply(&mut workspaces);
        return Ok(());
    }

    pub async fn update_workspace(&mut self, workspace_id: usize, data: &WorkspaceUpdateData) -> Result<()> {
        let workspace = match self.get_workspace_mut(workspace_id) {
            Some(ws) => ws,
            _ => anyhow::bail!("Workspace with id {} not found", workspace_id),
        };
        if let Some(name) = &data.name {
            workspace.name = name.clone();
        }
        if let Some(compiler_id) = &data.compiler {
            if !compiler_exists(compiler_id).await {
                anyhow::bail!("Compiler not found: {}", compiler_id);
            }
            workspace.compiler_id = compiler_id.clone();
        }
        return Ok(());
    }

    pub fn set_group_project(&mut self, groupproj_path: &String) -> Result<()> {
        let path = PathBuf::from(groupproj_path);
        if !path.exists() {
            anyhow::bail!("Group project file does not exist: {}", groupproj_path);
        }
        let mut group_project = GroupProject {
            name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("<name error>").to_string(),
            project_links: Vec::new(),
            path: groupproj_path.clone(),
            active_configuration: None,
            active_platform: None,
        };
        group_project.fill(self)?;
        self.group_project = Some(group_project);
        return Ok(());
    }

    pub fn remove_group_project(&mut self) {
        self.group_project = None;

        let linked_project_ids: HashSet<usize> = self.workspaces
            .iter()
            .flat_map(|workspace| workspace.project_links.iter())
            .map(|link| link.project_id)
            .collect();

        self.projects.retain(|project| linked_project_ids.contains(&project.id));

        if let Some(active_project_id) = self.active_project_id && !self.can_find_any_links(active_project_id) {
            self.active_project_id = None;
        }
    }

    pub fn get_project(&self, project_id: usize) -> Option<&Project> {
        return self.projects.iter().find(|proj| proj.id == project_id);
    }

    pub fn get_project_mut(&mut self, project_id: usize) -> Option<&mut Project> {
        return self.projects.iter_mut().find(|proj| proj.id == project_id);
    }

    pub fn get_workspace(&self, workspace_id: usize) -> Option<&Workspace> {
        return self.workspaces.iter().find(|ws| ws.id == workspace_id);
    }

    pub fn get_workspace_mut(&mut self, workspace_id: usize) -> Option<&mut Workspace> {
        return self.workspaces.iter_mut().find(|ws| ws.id == workspace_id);
    }

    pub fn get_workspace_index(&self, workspace_id: usize) -> Option<usize> {
        return self.workspaces.iter().position(|ws| ws.id == workspace_id);
    }

    pub fn find_project_by_dproj(&self, dproj: &String) -> Option<&Project> {
        return self.projects.iter().find(|proj| proj.dproj.as_ref().map_or(false, |p| p == dproj));
    }

    pub fn sort(&mut self) {
        self.workspaces.sort_by(|a: &Workspace, b: &Workspace| a.sort_rank.cmp(&b.sort_rank));
        for workspace in &mut self.workspaces {
            workspace.project_links.sort_by(|a: &ProjectLink, b: &ProjectLink| a.sort_rank.cmp(&b.sort_rank));
        }
        if let Some(group_project) = &mut self.group_project {
            group_project.project_links.sort_by(|a: &ProjectLink, b: &ProjectLink| a.sort_rank.cmp(&b.sort_rank));
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        if let Some(active_id) = self.active_project_id {
            return self.projects.iter().find(|proj| proj.id == active_id);
        }
        return None;
    }

    pub fn projects_of_workspace(&self, workspace: &Workspace) -> Vec<&Project> {
        let mut result = Vec::new();
        for project_link in &workspace.project_links {
            if let Some(project) = self.projects.iter().find(|proj| proj.id == project_link.project_id) {
                result.push(project);
            }
        }
        return result;
    }

    pub fn projects_of_group_project(&self, group_project: &GroupProject) -> Vec<&Project> {
        let mut result = Vec::new();
        for project_link in &group_project.project_links {
            if let Some(project) = self.projects.iter().find(|proj| proj.id == project_link.project_id) {
                result.push(project);
            }
        }
        return result;
    }
}

impl FilePath for ProjectsData {
    fn get_file_path() -> &'static PathBuf {
        lazy_static::lazy_static! {
            static ref PATH: PathBuf = {
                dirs::config_dir()
                    .expect("Could not determine config directory")
                    .join("ddk")
                    .join("projects.ron")
            };
        }
        return &PATH;
    }
}

impl Load for ProjectsData {}