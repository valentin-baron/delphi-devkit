use serde::{Serialize, Deserialize};
use crate::lexorank::HasLexoRank;
use crate::lexorank::LexoRank;
use super::*;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: usize,
    pub name: String,
    pub compiler_id: String,
    pub project_links: Vec<ProjectLink>,
    pub sort_rank: LexoRank,
}

impl Workspace {
    pub fn new(id: usize, name: String, compiler_id: String, lexo_rank: LexoRank) -> Self {
        Workspace {
            id,
            name,
            compiler_id,
            project_links: Vec::new(),
            sort_rank: lexo_rank,
        }
    }
}

impl HasLexoRank for Workspace {
    fn get_lexorank(&self) -> &LexoRank {
        &self.sort_rank
    }
    fn set_lexorank(&mut self, lexorank: LexoRank) {
        self.sort_rank = lexorank;
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