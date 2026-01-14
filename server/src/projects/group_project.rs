use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use crate::projects::*;
use crate::files::groupproj::parse_groupproj;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct GroupProject {
    pub name: String,
    pub path: String,
    pub compiler_id: String,
    pub project_links: Vec<ProjectLink>,
}

impl GroupProject {
    pub fn compiler(&self) -> Option<CompilerConfiguration> {
        let mut compilers = CompilerConfigurations::new();
        return compilers.remove(&self.compiler_id.to_string());
    }

    pub fn fill(&mut self, projects_data: &mut ProjectsData) -> Result<()> {
        let project_paths = parse_groupproj(PathBuf::from(&self.path))?;
        for project_path in project_paths {
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
                };
                project.discover_paths()?;
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