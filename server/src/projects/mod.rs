pub mod compilers;
pub mod project_data;
pub mod changes;
pub mod workspace;
pub mod project;
pub mod group_project;
pub mod file_watch;

use anyhow::Result;
use serde_json::Value;
use crate::{EventDone, lexorank::{HasLexoRank, LexoRank}, utils::FileLock};

pub use compilers::*;
pub use project_data::*;
pub use changes::*;
pub use workspace::*;
pub use project::*;
pub use group_project::*;
pub use file_watch::*;

pub trait Named {
    fn get_name(&self) -> &String;
}

pub trait ProjectLinkContainer: Named {
    fn get_project_links(&self) -> &Vec<ProjectLink>;
    fn get_project_links_mut(&mut self) -> &mut Vec<ProjectLink>;

    fn new_project_link(&mut self, id: usize, project_id: usize) {
        let links = self.get_project_links_mut();
        let last_rank = if let Some(last_link) = links.last() {
            last_link.sort_rank.clone()
        } else {
            LexoRank::default()
        };
        links.push(ProjectLink {
            id,
            project_id,
            sort_rank: last_rank.next(),
        });
    }

    fn index_of(&self, project_link_id: usize) -> Option<usize> {
        return self.get_project_links().iter().position(|link| link.id == project_link_id);
    }

    fn move_project_link(&mut self, project_link_id: usize, drop_link_id: Option<usize>) -> Result<()> {
        let links = self.get_project_links();
        let target_index = match drop_link_id {
            Some(id) => self.index_of(id)
                .ok_or_else(|| anyhow::anyhow!("Drop target link with id {} not found in {}", id, self.get_name()))?,
            _ => links.len(),
        };
        if target_index > links.len() {
            anyhow::bail!("Target index {} out of bounds", target_index);
        }
        let link = self.export_project_link(project_link_id)?;
        return self.import_project_link(link, drop_link_id);
    }

    fn export_project_link(&mut self, project_link_id: usize) -> Result<ProjectLink> {
        let index = self.index_of(project_link_id)
            .ok_or_else(|| anyhow::anyhow!("Project link with id {} not found in {}", project_link_id, self.get_name()))?;
        let links = self.get_project_links_mut();
        if index >= links.len() {
            anyhow::bail!("Source index {} out of bounds", index);
        }
        let project_link = links.remove(index);
        Ok(project_link)
    }

    fn import_project_link(&mut self, project_link: ProjectLink, drop_link_id: Option<usize>) -> Result<()> {
        let links = self.get_project_links();
        let target_index = match drop_link_id {
            Some(id) => self.index_of(id)
                .ok_or_else(|| anyhow::anyhow!("Drop target link with id {} not found in {}", id, self.get_name()))?,
            _ => links.len(),
        };
        if target_index > links.len() {
            anyhow::bail!("Target index {} out of bounds", target_index);
        }
        self.get_project_links_mut().insert(target_index, project_link);
        self.reorder_links();
        Ok(())
    }

    fn reorder_links(&mut self) {
        let mut items: Vec<&mut dyn HasLexoRank> = self.get_project_links_mut().iter_mut().map(|link| link as &mut dyn HasLexoRank).collect();
        LexoRank::apply(&mut items);
    }
}

pub async fn update(json: Value, client: tower_lsp::Client) -> Result<()> {
    if let Some(inner) = json.get("projectsData") {
        let mut file_lock: FileLock<ProjectsData> = FileLock::new()?;
        file_lock.file = serde_json::from_value(inner.clone())?;
        file_lock.file.validate()?;
        file_lock.file.save()?;
        EventDone::notify_json(&client, &json).await;
        return Ok(());
    }
    if let Some(inner) = json.get("changeSet") {
        let change_set: ChangeSet = serde_json::from_value(inner.clone())?;
        change_set.execute(&client).await?;
        return Ok(());
    }
    if let Some(inner) = json.get("compilerConfigurations") {
        let file_lock: FileLock<CompilerConfigurations> = FileLock::new()?;
        let mut compilers = file_lock.file;
        let compiler_configurations: CompilerConfigurations =
            serde_json::from_value(inner.clone())?;
        compilers.overwrite(compiler_configurations);
        compilers.validate()?;
        compilers.save()?;
        EventDone::notify_json(&client, &json).await;
        return Ok(());
    }
    anyhow::bail!("No valid data found to update projects.");
}