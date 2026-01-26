use serde::{Deserialize, Serialize};
use anyhow::Result;

use crate::{EventDone, projects::*};

#[derive(Serialize, Deserialize)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
    pub event_id: String,
}

impl ChangeSet {
    pub async fn execute(self, client: &tower_lsp::Client) -> Result<()> {
        for change in self.changes {
            change.execute()?;
        }
        EventDone::notify(client, self.event_id).await;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceUpdateData {
    pub name: Option<String>,
    pub compiler: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectUpdateData {
    pub name: Option<String>,
    pub directory: Option<String>,
    pub dproj: Option<String>,
    pub dpr: Option<String>,
    pub dpk: Option<String>,
    pub exe: Option<String>,
    pub ini: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Change {
    NewProject { file_path: String, workspace_id: usize },
    AddProject { project_id: usize, workspace_id: usize },
    RemoveProject { project_link_id: usize },
    MoveProject { project_link_id: usize, drop_target: usize },
    RefreshProject { project_id: usize },
    UpdateProject { project_id: usize, data: ProjectUpdateData },
    SelectProject { project_id: usize },
    AddWorkspace { name: String, compiler: String },
    RemoveWorkspace { workspace_id: usize },
    MoveWorkspace { workspace_id: usize, drop_target: usize },
    UpdateWorkspace { workspace_id: usize, data: WorkspaceUpdateData },
    AddCompiler { key: String, config: CompilerConfiguration },
    RemoveCompiler { compiler: String },
    UpdateCompiler { key: String, data: PartialCompilerConfiguration },
    SetGroupProject { groupproj_path: String },
    RemoveGroupProject,
    SetGroupProjectCompiler { compiler: String },
}

impl Change {
    pub fn execute(self) -> Result<()> {
        match self {
            Change::NewProject { file_path, workspace_id } => {
                return Self::new_project(file_path, workspace_id);
            }
            Change::AddProject { project_id, workspace_id } => {
                return Self::add_project_link(project_id, workspace_id);
            }
            Change::RemoveProject { project_link_id } => {
                return Self::remove_project_link(project_link_id);
            }
            Change::MoveProject { project_link_id, drop_target } => {
                return Self::move_project(project_link_id, drop_target);
            }
            Change::RefreshProject { project_id } => {
                return Self::refresh_project(project_id);
            }
            Change::UpdateProject { project_id, data } => {
                return Self::update_project(project_id, data);
            }
            Change::SelectProject { project_id } => {
                return Self::select_project(project_id);
            }
            Change::AddWorkspace { name, compiler } => {
                return Self::add_workspace(name, compiler);
            }
            Change::RemoveWorkspace { workspace_id } => {
                return Self::remove_workspace(workspace_id);
            }
            Change::MoveWorkspace { workspace_id, drop_target } => {
                return Self::move_workspace(workspace_id, drop_target);
            }
            Change::UpdateWorkspace { workspace_id, data } => {
                return Self::update_workspace(workspace_id, data);
            }
            Change::AddCompiler { key, config } => {
                return Self::add_compiler(key, config);
            }
            Change::RemoveCompiler { compiler } => {
                return Self::remove_compiler(compiler);
            }
            Change::UpdateCompiler { key, data } => {
                return Self::update_compiler(key, data);
            }
            Change::SetGroupProject { groupproj_path} => {
                return Self::set_group_project(groupproj_path);
            }
            Change::RemoveGroupProject => {
                return Self::remove_group_project();
            }
            Change::SetGroupProjectCompiler { compiler } => {
                return Self::set_group_project_compiler(compiler);
            }
        }
    }

    fn new_project(file_path: String, workspace_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.new_project(&file_path, workspace_id)?;
        return projects_data.save();
    }

    fn add_project_link(project_id: usize, workspace_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.add_project_link(project_id, workspace_id)?;
        return projects_data.save();
    }

    fn remove_project_link(project_link_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.remove_project_link(project_link_id);
        return projects_data.save();
    }

    fn move_project(project_link_id: usize, drop_target: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.move_project_link(project_link_id, drop_target)?;
        return projects_data.save();
    }

    fn refresh_project(project_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.refresh_project_paths(project_id)?;
        return projects_data.save();
    }

    fn select_project(project_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.select_project(project_id)?;
        return projects_data.save();
    }

    fn update_project(project_id: usize, data: ProjectUpdateData) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.update_project(project_id, data)?;
        return projects_data.save();
    }

    fn add_workspace(name: String, compiler: String) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.new_workspace(&name, &compiler)?;
        return projects_data.save();
    }

    fn remove_workspace(workspace_id: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.remove_workspace(workspace_id);
        return projects_data.save();
    }

    fn move_workspace(workspace_id: usize, drop_target: usize) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.move_workspace(workspace_id, drop_target)?;
        return projects_data.save();
    }

    fn update_workspace(workspace_id: usize, data: WorkspaceUpdateData) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.update_workspace(workspace_id, &data)?;
        return projects_data.save();
    }

    fn add_compiler(key: String, config: CompilerConfiguration) -> Result<()> {
        let mut file_lock: FileLock<CompilerConfigurations> = FileLock::new()?;
        let compilers = &mut file_lock.file;
        compilers.insert(key, config);
        return compilers.save();
    }

    fn remove_compiler(compiler: String) -> Result<()> {
        let file_lock: FileLock<CompilerConfigurations> = FileLock::new()?;
        let mut compilers = file_lock.file;
        if compilers.remove(&compiler).is_none() {
            anyhow::bail!("Unable to remove compiler - compiler not found: {}", compiler);
        }
        return compilers.save();
    }

    fn update_compiler(key: String, data: PartialCompilerConfiguration) -> Result<()> {
        let file_lock: FileLock<CompilerConfigurations> = FileLock::new()?;
        let mut compilers = file_lock.file;
        if let Some(compiler) = compilers.get_mut(&key) {
            compiler.update(&data);
            return compilers.save();
        } else {
            anyhow::bail!("Unable to update compiler - compiler not found: {}", key);
        }
    }

    fn set_group_project(groupproj_path: String) -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.set_group_project(&groupproj_path)?;
        return projects_data.save();
    }

    fn remove_group_project() -> Result<()> {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.remove_group_project();
        return projects_data.save();
    }

    fn set_group_project_compiler(compiler: String) -> Result<()> {
        if !compiler_exists(&compiler) {
            anyhow::bail!(
                "Unable to set group project compiler - compiler not found: {}",
                compiler
            );
        }
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        let projects_data = &mut file_lock.file;
        projects_data.group_project_compiler_id = compiler.clone();
        return projects_data.save();
    }
}
