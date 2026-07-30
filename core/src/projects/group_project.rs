use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use crate::projects::*;
use crate::files::groupproj::parse_groupproj;
use crate::utils::normalize_path;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupProject {
    pub name: String,
    pub path: String,
    pub project_links: Vec<ProjectLink>,
    /// Group-project-level configuration override.
    pub active_configuration: Option<String>,
    /// Group-project-level platform override.
    pub active_platform: Option<String>,
}

impl Default for GroupProject {
    fn default() -> Self {
        GroupProject {
            name: String::new(),
            path: String::new(),
            project_links: Vec::new(),
            active_configuration: None,
            active_platform: None,
        }
    }
}

impl GroupProject {
    pub fn fill(&mut self, projects_data: &mut ProjectsData, ide_env: &[(String, String)]) -> Result<()> {
        let project_paths = parse_groupproj(PathBuf::from(&self.path))?;
        for project_path in project_paths {
            let project_path = normalize_path(&project_path);
            let dproj = project_path.to_string_lossy().to_string();
            let existing_project_id = projects_data.find_project_by_dproj(&dproj).map(|p| p.id);
            if let Some(existing_id) = existing_project_id {
                self.new_project_link(projects_data.next_id(), existing_id);
                continue;
            } else {
                let project_id = projects_data.next_id();
                let mut project = Project {
                    id: project_id,
                    name: project_path.file_stem().and_then(|s| s.to_str()).unwrap_or("<name error>").to_string(),
                    directory: project_path.parent().and_then(|p| p.to_str()).unwrap_or("<directory error>").to_string(),
                    dproj: Some(dproj.clone()),
                    dpr: None,
                    dpk: None,
                    exe: None,
                    ini: None,
                    active_configuration: None,
                    active_platform: None,
                    start_parameters: None,
                    dproj_run_params: None,
                    dproj_host_application: None,
                    host_application: None,
                };
                project.discover_paths(ide_env)?;
                projects_data.projects.push(project);
                self.new_project_link(projects_data.next_id(), project_id);
            }
        }
        return Ok(());
    }
}

impl Named for GroupProject {
    fn get_name(&self) -> &String {
        return &self.name;
    }
}

impl ProjectLinkContainer for GroupProject {
    fn get_project_links(&self) -> &Vec<ProjectLink> {
        return &self.project_links;
    }
    fn get_project_links_mut(&mut self) -> &mut Vec<ProjectLink> {
        return &mut self.project_links;
    }
}