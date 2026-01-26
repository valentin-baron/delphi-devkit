use super::*;
use crate::{CompileProjectParams, CompilerProgress, defer_async};
use anyhow::Result;
use scopeguard::defer;
use tokio::io::{AsyncBufReadExt, BufReader};
use std::process::Stdio;
use tokio::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::path::PathBuf;

pub struct Compiler {
    client: tower_lsp::Client,
    params: CompileProjectParams,
    projects_data: ProjectsData,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SUCCESS: AtomicBool = AtomicBool::new(false);
static CODE: AtomicIsize = AtomicIsize::new(-1);

impl Compiler {
    pub fn new(client: tower_lsp::Client, params: CompileProjectParams) -> Self {
        Compiler {
            client,
            params,
            projects_data: FileLock::<ProjectsData>::read_only_copy(),
        }
    }

    pub async fn compile(&self) -> Result<()> {
        if ACTIVE.load(Ordering::SeqCst) {
            anyhow::bail!("Another compilation is already in progress. Please wait until it finishes.");
        }
        ACTIVE.store(true, Ordering::SeqCst);
        defer! {
            ACTIVE.store(false, Ordering::SeqCst);
        }
        let (projects, configuration, do_rebuild, single, header, footer);
        match self.params {
            CompileProjectParams::Project { project_link_id, project_id, rebuild } => {
                do_rebuild = rebuild;
                single = true;
                let project = self.projects_data.get_project(project_id).ok_or_else(|| anyhow::anyhow!("Project with id {} not found", project_id))?;
                if let Some(link_id) = project_link_id {
                    if self.projects_data.is_project_link_in_group_project(link_id) {
                        configuration = self.projects_data.group_projects_compiler();
                    } else if let Some(workspace_id) = self.projects_data.get_workspace_id_containing_project_link(link_id) {
                        let workspace = self
                            .projects_data
                            .get_workspace(workspace_id)
                            .ok_or_else(|| anyhow::anyhow!("Workspace with id {} not found", workspace_id))?;
                        configuration = workspace.compiler();
                    } else {
                        anyhow::bail!("No workspace or group project contains project link with id {}", link_id);
                    }
                } else {
                    let workspace_id = self.projects_data.workspaces.iter()
                        .find_map(|ws| if ws.project_links.iter().any(|pl| pl.project_id == project_id) { Some(ws.id) } else { None })
                        .ok_or_else(|| anyhow::anyhow!("No workspace contains project link with project id {}", project_id))?;
                    configuration = self.projects_data.get_workspace(workspace_id)
                        .ok_or_else(|| anyhow::anyhow!("Workspace with id {} not found", workspace_id))?
                        .compiler();
                }
                projects = vec![project];
                let target = project.get_project_file()?;
                header = CompHeader::new(
                    "Project".to_string(),
                    project.name.clone(),
                    target.to_string_lossy().to_string(),
                    configuration.product_name.clone(),
                    rebuild,
                );
                footer = CompFooter::new(
                    "Project".to_string(),
                    project.name.clone(),
                    target.to_string_lossy().to_string(),
                    configuration.product_name.clone(),
                    rebuild,
                    Box::new(|| {
                        // Determine success based on compilation result
                        SUCCESS.load(Ordering::SeqCst)
                    }),
                );
            },
            CompileProjectParams::AllInWorkspace { workspace_id, rebuild } => {
                do_rebuild = rebuild;
                single = false;
                let workspace = match self.projects_data.get_workspace(workspace_id) {
                    Some(ws) => ws,
                    _ => anyhow::bail!("Workspace with id {} not found", workspace_id),
                };
                configuration = workspace.compiler();
                projects = workspace.project_links.iter()
                    .map(|link| {
                        self.projects_data
                            .get_project(link.project_id)
                            .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
                    })
                    .collect::<Result<Vec<_>>>()?;
                header = CompHeader::new(
                    "Workspace".to_string(),
                    workspace.name.clone(),
                    format!("Projects of Workspace '{}'", workspace.name),
                    configuration.product_name.clone(),
                    rebuild,
                );
                footer = CompFooter::new(
                    "Workspace".to_string(),
                    workspace.name.clone(),
                    format!("Projects of Workspace '{}'", workspace.name),
                    configuration.product_name.clone(),
                    rebuild,
                    Box::new(|| {
                        // Determine success based on compilation result
                        SUCCESS.load(Ordering::SeqCst)
                    }),
                );
            },
            CompileProjectParams::AllInGroupProject { rebuild } => {
                do_rebuild = rebuild;
                single = false;
                let group_project = match &self.projects_data.group_project {
                    Some(gp) => gp,
                    _ => anyhow::bail!("No group project defined"),
                };
                configuration = self.projects_data.group_projects_compiler();
                projects = group_project.project_links.iter()
                    .map(|link| {
                        self.projects_data
                            .get_project(link.project_id)
                            .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
                    })
                    .collect::<Result<Vec<_>>>()?;
                header = CompHeader::new(
                    "Group Project".to_string(),
                    group_project.name.clone(),
                    format!("Projects of Group Project '{}'", group_project.name),
                    configuration.product_name.clone(),
                    rebuild,
                );
                footer = CompFooter::new(
                    "Group Project".to_string(),
                    group_project.name.clone(),
                    format!("Projects of Group Project '{}'", group_project.name),
                    configuration.product_name.clone(),
                    rebuild,
                    Box::new(|| {
                        // Determine success based on compilation result
                        SUCCESS.load(Ordering::SeqCst)
                    }),
                );
            },
            CompileProjectParams::FromLink { project_link_id, rebuild } => {
                do_rebuild = rebuild;
                single = false;
                if let Some(workspace_id) = self.projects_data.get_workspace_id_containing_project_link(project_link_id) {
                    let workspace = self.projects_data.get_workspace(workspace_id)
                        .ok_or_else(|| anyhow::anyhow!("Workspace with id {} not found", workspace_id))?;
                    if let Some(index) = workspace.index_of(project_link_id) {
                        projects = workspace.project_links[index..]
                            .iter()
                            .map(|link| {
                                self.projects_data
                                    .get_project(link.project_id)
                                    .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        configuration = workspace.compiler();
                        let project_name = projects.first().map(|p| p.name.clone()).unwrap_or("<unknown>".to_string());
                        header = CompHeader::new(
                            format!("Workspace '{}'", workspace.name),
                            format!("Project {project_name}"),
                            format!("Projects of Workspace '{}' from project {project_name}", workspace.name),
                            configuration.product_name.clone(),
                            rebuild,
                        );
                        footer = CompFooter::new(
                            format!("Workspace '{}'", workspace.name),
                            format!("Project {project_name}"),
                            format!("Projects of Workspace '{}' from project {project_name}", workspace.name),
                            configuration.product_name.clone(),
                            rebuild,
                            Box::new(|| {
                                // Determine success based on compilation result
                                SUCCESS.load(Ordering::SeqCst)
                            }),
                        );
                    } else {
                        anyhow::bail!("Project link with id {} not found in workspace {}", project_link_id, workspace.name);
                    }
                } else if let Some(group_project) = &self.projects_data.group_project {
                    if let Some(index) = group_project.index_of(project_link_id) {
                        projects = group_project.project_links[index..]
                            .iter()
                            .map(|link| {
                                self.projects_data
                                    .get_project(link.project_id)
                                    .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        configuration = self.projects_data.group_projects_compiler();
                        let project_name = projects.first().map(|p| p.name.clone()).unwrap_or("<unknown>".to_string());
                        header = CompHeader::new(
                            format!("Group Project '{}'", group_project.name),
                            format!("Project {project_name}"),
                            format!("Projects of Group Project '{}' from project {project_name}", group_project.name),
                            configuration.product_name.clone(),
                            rebuild,
                        );
                        footer = CompFooter::new(
                            format!("Group Project '{}'", group_project.name),
                            format!("Project {project_name}"),
                            format!("Projects of Group Project '{}' from project {project_name}", group_project.name),
                            configuration.product_name.clone(),
                            rebuild,
                            Box::new(|| {
                                // Determine success based on compilation result
                                SUCCESS.load(Ordering::SeqCst)
                            }),
                        );
                    } else {
                        anyhow::bail!("Project link with id {} not found in group project {}", project_link_id, group_project.name);
                    }
                } else {
                    anyhow::bail!("No workspace or group project contains project link with id {}", project_link_id);
                }
            },
        };
        let parameters = CompilationParameters {
            projects,
            configuration,
            rebuild: do_rebuild,
            single,
            header,
            footer,
        };
        self.start(&parameters).await?;
        self.do_compile(&parameters).await?;
        self.finish(&parameters).await?;
        return Ok(());
    }

    async fn start(&self, parameters: &CompilationParameters<'_>) -> Result<()> {
        CompilerProgress::notify_start(&self.client, parameters.header.into_vec()).await;
        Ok(())
    }

    async fn finish(&self, parameters: &CompilationParameters<'_>) -> Result<()> {
        CompilerProgress::notify_completed(
            &self.client,
            SUCCESS.load(Ordering::SeqCst),
            CODE.load(Ordering::SeqCst),
            parameters.footer.into_vec()
        ).await;
        Ok(())
    }

    async fn do_compile(&self, parameters: &CompilationParameters<'_>) -> Result<()> {
        for project in &parameters.projects {
            let client_deferred = self.client.clone();
            let project_id = project.id;
            let single_project = parameters.single;
            let single_project_footer = SingleProjectCompFooter::new(
                parameters.rebuild,
                parameters.configuration.product_name.clone(),
                project.name.clone(),
                project.get_project_file()?.to_string_lossy().to_string(),
                Box::new(|| {
                    // Determine success based on compilation result
                    SUCCESS.load(Ordering::SeqCst)
                }),
            );

            defer_async! {
                if single_project {
                    CompilerProgress::notify_single_project_completed(
                        &client_deferred,
                        project_id,
                        SUCCESS.load(Ordering::SeqCst),
                        CODE.load(Ordering::SeqCst),
                        single_project_footer.into_vec()
                    ).await
                }
            }

            let rsvars_path = PathBuf::from(&parameters.configuration.installation_path).join("bin").join("rsvars.bat");
            if !rsvars_path.exists() {
                anyhow::bail!("Cannot find rsvars.bat at path: {}", rsvars_path.to_string_lossy());
            }
            let rsvars_path = rsvars_path.to_string_lossy();
            let project_file = project.get_project_file()?;
            let args = format!(
                "/t:Clean,{} {}",
                if parameters.rebuild { "Build" } else { "Make" },
                parameters.configuration.build_arguments.join(" ")
            );
            let mut child_process = Command::new("cmd")
                .args([
                    "/C",
                    format!(
                        "call {rsvars_path} && msbuild \"{}\" {args}",
                        project_file.to_string_lossy()
                    ).as_str(),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;

            let stdout = child_process.stdout.take().unwrap();
            let stderr = child_process.stderr.take().unwrap();

            let mut out_lines = BufReader::new(stdout).lines();
            let mut err_lines = BufReader::new(stderr).lines();

            let stdout_client = self.client.clone();
            let stderr_client = self.client.clone();

            let stdout_compiler_name = parameters.configuration.product_name.clone();
            let stderr_compiler_name = parameters.configuration.product_name.clone();

            let stdout_task = tokio::spawn(async move {
                while let Ok(Some(line)) = out_lines.next_line().await {
                    if let Some(diagnostic) = CompilerLineDiagnostic::from_line(&line) {
                        diagnostic.publish(&stdout_client, &stdout_compiler_name).await;
                        CompilerProgress::notify_stdout(&stdout_client, format!("{diagnostic}")).await;
                        continue;
                    }
                    CompilerProgress::notify_stdout(&stdout_client, line).await;
                }
            });

            let stderr_task = tokio::spawn(async move {
                while let Ok(Some(line)) = err_lines.next_line().await {
                    if let Some(diagnostic) = CompilerLineDiagnostic::from_line(&line) {
                        diagnostic.publish(&stderr_client, &stderr_compiler_name).await;
                        CompilerProgress::notify_stderr(&stderr_client, format!("{diagnostic}")).await;
                        continue;
                    }
                    CompilerProgress::notify_stderr(&stderr_client, line).await;
                }
            });

            let status = child_process.wait().await?;
            stdout_task.await?;
            stderr_task.await?;
            SUCCESS.store(status.success(), Ordering::SeqCst);
            CODE.store(status.code().unwrap_or(-1) as isize, Ordering::SeqCst);

        }
        return Ok(());
    }
}


fn format_line(text: &str, total_width: usize) -> String {
    let padding = total_width.saturating_sub(text.len() + 2);
    if padding == 0 {
        return text.to_string();
    }
    let left_padding = padding / 2;
    format!(" {}{}", " ".repeat(left_padding), text)
}

struct CompilationParameters<'compiler> {
    projects: Vec<&'compiler Project>,
    configuration: CompilerConfiguration,
    rebuild: bool,
    single: bool,
    header: CompHeader,
    footer: CompFooter,
}

unsafe impl Send for CompilationParameters<'_> {}
unsafe impl Sync for CompilationParameters<'_> {}

struct CompHeader {
    entity_type: String,
    entity_name: String,
    target: String,
    compiler_name: String,
    rebuild: bool,
}

unsafe impl Send for CompHeader {}
unsafe impl Sync for CompHeader {}

impl CompHeader {
    fn new(entity_type: String, entity_name: String, target: String, compiler_name: String, rebuild: bool) -> Self {
        CompHeader {
            entity_type,
            entity_name,
            target,
            compiler_name,
            rebuild,
        }
    }

    fn into_vec(&self) -> Vec<String> {
        let topline = format_line(format!("Compiling {} {}", self.entity_type, self.entity_name).as_str(), 72);
        let target = format_line(format!("→ {} ←", self.target.as_str()).as_str(), 70);
        let compiler = format_line(format!("🛠️ Compiler: {}", self.compiler_name).as_str(), 70);
        let action_str = if self.rebuild { "Rebuild (Clean,Build)" } else { "Compile (Clean,Make)" };
        let action = format_line(format!("🗲 Action: {}", action_str).as_str(), 70);
        vec![
            "╒══════════════════════════════════════════════════════════════════════╕".to_string(),
            topline,
            target,
            compiler,
            action,
            "╘══════════════════════════════════════════════════════════════════════╛".to_string(),
        ]
    }
}

struct CompFooter {
    entity_type: String,
    entity_name: String,
    target: String,
    compiler_name: String,
    rebuild: bool,
    success: Box<dyn Fn() -> bool>,
}

unsafe impl Send for CompFooter {}
unsafe impl Sync for CompFooter {}

impl CompFooter {
    fn new(entity_type: String, entity_name: String, target: String, compiler_name: String, rebuild: bool, success: Box<dyn Fn() -> bool>) -> Self {
        CompFooter {
            entity_type,
            entity_name,
            target,
            compiler_name,
            rebuild,
            success,
        }
    }

    fn into_vec(&self) -> Vec<String> {
        let topline = format_line(format!("Compiling {} {}", self.entity_type, self.entity_name).as_str(), 72);
        let target = format_line(format!("→ {} ←", self.target.as_str()).as_str(), 70);
        let compiler = format_line(format!("🛠️ Compiler: {}", self.compiler_name).as_str(), 70);
        let action_str = if self.rebuild { "Rebuild (Clean,Build)" } else { "Compile (Clean,Make)" };
        let action = format_line(format!("🗲 Action: {}", action_str).as_str(), 70);
        let status_str = if (self.success)() { "✅ SUCCESS" } else { "❌ FAILED" };
        let status = format_line(format!("Status: {}", status_str).as_str(), 70);
        vec![
            "╒══════════════════════════════════════════════════════════════════════╕".to_string(),
            topline,
            target,
            compiler,
            action,
            status,
            "╘══════════════════════════════════════════════════════════════════════╛".to_string(),
        ]
    }
}

struct SingleProjectCompFooter {
    rebuild: bool,
    compiler_name: String,
    project_name: String,
    target: String,
    success: Box<dyn Fn() -> bool>,
}

unsafe impl Send for SingleProjectCompFooter {}
unsafe impl Sync for SingleProjectCompFooter {}

impl SingleProjectCompFooter {
    fn new(rebuild: bool, compiler_name: String, project_name: String, target: String, success: Box<dyn Fn() -> bool>) -> Self {
        SingleProjectCompFooter {
            rebuild,
            compiler_name,
            project_name,
            target,
            success,
        }
    }

    fn into_vec(&self, ) -> Vec<String> {
        let topline = format_line(format!("Compiling Project: {}", self.project_name).as_str(), 72);
        let target = format_line(&format!("→ {} ←", self.target), 70);
        let compiler = format_line(&format!("🛠️ Compiler: {}", self.compiler_name), 70);
        let action_str = if self.rebuild { "Rebuild (Clean,Build)" } else { "Compile (Clean,Make)" };
        let action = format_line(&format!("🗲 Action: {}", action_str), 70);
        let status_str = if (self.success)() { "✅ SUCCESS" } else { "❌ FAILED" };
        let status = format_line(&format!("Status: {}", status_str), 70);
        vec![
            "╒══════════════════════════════════════════════════════════════════════╕".to_string(),
            topline,
            target,
            compiler,
            action,
            status,
            "╘══════════════════════════════════════════════════════════════════════╛".to_string(),
        ]
    }
}