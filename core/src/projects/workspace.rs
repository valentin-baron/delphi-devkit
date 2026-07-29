use serde::{Serialize, Deserialize};
use super::*;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub id: usize,
    pub name: String,
    pub compiler_id: String,
    pub project_links: Vec<ProjectLink>,
    /// Workspace-level configuration override.  When set, all linked projects
    /// inherit this value (unless individually overridden).
    pub active_configuration: Option<String>,
    /// Workspace-level platform override.
    pub active_platform: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            id: 0,
            name: String::new(),
            compiler_id: String::from("12.0"),
            project_links: Vec::new(),
            active_configuration: None,
            active_platform: None,
        }
    }
}

impl Workspace {
    pub fn new(id: usize, name: String, compiler_id: String) -> Self {
        Workspace {
            id,
            name,
            compiler_id,
            project_links: Vec::new(),
            active_configuration: None,
            active_platform: None,
        }
    }

    pub async fn compiler(&self) -> CompilerConfiguration {
        let compilers = COMPILER_CONFIGURATIONS.read().await;
        if let Some(compiler) = compilers.get(&self.compiler_id.to_string()) {
            return compiler.clone();
        }
        return compilers
            .get("12.0")
            .expect(format!(
                "Compiler with id {} not found; should not be possible.",
                self.compiler_id).as_str())
            .clone();
    }
}

impl Named for Workspace {
    fn get_name(&self) -> &String {
        return &self.name;
    }
}

impl ProjectLinkContainer for Workspace {
    fn get_project_links(&self) -> &Vec<ProjectLink> {
        return &self.project_links;
    }
    fn get_project_links_mut(&mut self) -> &mut Vec<ProjectLink> {
        return &mut self.project_links;
    }
}