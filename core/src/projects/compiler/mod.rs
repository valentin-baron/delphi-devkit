pub mod compiler_state;

use super::*;
use crate::files::dproj as dproj_cache;
use crate::state::PROJECTS_DATA;
use crate::{CompileProjectParams, CompilerProgress};
use anyhow::Result;
use rust_search::SearchBuilder;
use scopeguard::defer;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncRead, BufReader};
use tokio::process::Command;
use tower_lsp::lsp_types::{Diagnostic, Url};

#[derive(Debug, Clone)]
pub struct CompileResult {
    pub success: bool,
    pub cancelled: bool,
    pub code: i32,
}

pub struct Compiler {
    client: Option<tower_lsp::Client>,
    params: CompileProjectParams,
    projects_data: ProjectsData,
}

impl Compiler {
    /// Create a compiler with a live LSP client (used by ddk-server).
    pub async fn new(client: tower_lsp::Client, params: &CompileProjectParams) -> Self {
        Compiler {
            client: Some(client),
            params: params.clone(),
            projects_data: PROJECTS_DATA.read().await.clone(),
        }
    }

    /// Create a compiler without an LSP client (used by ddk-mcp-server).
    /// Progress is still broadcast via the in-process channel; diagnostics
    /// will not be published to VS Code.
    pub async fn new_standalone(params: &CompileProjectParams) -> Self {
        Compiler {
            client: None,
            params: params.clone(),
            projects_data: PROJECTS_DATA.read().await.clone(),
        }
    }

    async fn get_project_parameters<'a>(
        &'a self,
        project_id: usize,
        project_link_id: Option<usize>,
        rebuild: bool,
    ) -> Result<CompilationParameters<'a>> {
        let configuration;
        let project = self
            .projects_data
            .get_project(project_id)
            .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", project_id))?;
        if let Some(link_id) = project_link_id {
            if self.projects_data.is_project_link_in_group_project(link_id) {
                configuration = self.projects_data.group_projects_compiler().await;
            } else if let Some(workspace_id) = self
                .projects_data
                .get_workspace_id_containing_project_link(link_id)
            {
                let workspace =
                    self.projects_data
                        .get_workspace(workspace_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!("Workspace with id {} not found", workspace_id)
                        })?;
                configuration = workspace.compiler().await;
            } else {
                anyhow::bail!(
                    "No workspace or group project contains project link with id {}",
                    link_id
                );
            }
        } else {
            let workspace_id = self
                .projects_data
                .workspaces
                .iter()
                .find_map(|ws| {
                    if ws
                        .project_links
                        .iter()
                        .any(|pl| pl.project_id == project_id)
                    {
                        Some(ws.id)
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No workspace contains project link with project id {}",
                        project_id
                    )
                })?;
            configuration = self
                .projects_data
                .get_workspace(workspace_id)
                .ok_or_else(|| anyhow::anyhow!("Workspace with id {} not found", workspace_id))?
                .compiler().await;
        }
        let target = project.get_project_file()?;
        let compiler_name = configuration.product_name.clone();
        // Resolve config/platform for the banner
        let (eff_config, eff_platform) = if let Some(dproj_path) = &project.dproj {
            if let Ok(dproj_obj) = dproj_cache::get_or_load(project.id, &PathBuf::from(dproj_path)) {
                project.effective_config_platform(&dproj_obj)
            } else {
                (project.active_configuration.clone().unwrap_or_else(|| "Debug".to_string()),
                 project.active_platform.clone().unwrap_or_else(|| "Win32".to_string()))
            }
        } else {
            (project.active_configuration.clone().unwrap_or_else(|| "Debug".to_string()),
             project.active_platform.clone().unwrap_or_else(|| "Win32".to_string()))
        };
        return Ok(CompilationParameters {
            projects: vec![project],
            configuration,
            rebuild,
            only_one_project: true,
            banner: CompBanner::new(
                format!("Compiling Project {}", project.name),
                target.to_string_lossy().to_string(),
                compiler_name,
                rebuild,
            ).with_config_platform(eff_config, eff_platform),
        });
    }

    async fn get_all_workspace_parameters<'a>(
        &'a self,
        workspace_id: usize,
        rebuild: bool,
    ) -> Result<CompilationParameters<'a>> {
        let workspace = match self.projects_data.get_workspace(workspace_id) {
            Some(ws) => ws,
            _ => anyhow::bail!("Workspace with id {} not found", workspace_id),
        };
        let configuration = workspace.compiler().await;
        let projects = workspace
            .project_links
            .iter()
            .map(|link| {
                self.projects_data
                    .get_project(link.project_id)
                    .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
            })
            .collect::<Result<Vec<_>>>()?;
        let compiler_name = configuration.product_name.clone();
        return Ok(CompilationParameters {
            projects,
            configuration,
            rebuild,
            only_one_project: false,
            banner: CompBanner::new(
                format!("Compiling Workspace {}", workspace.name),
                format!("Projects of Workspace '{}'", workspace.name),
                compiler_name,
                rebuild,
            ),
        });
    }

    async fn get_all_group_project_parameters<'a>(
        &'a self,
        rebuild: bool,
    ) -> Result<CompilationParameters<'a>> {
        let group_project = match &self.projects_data.group_project {
            Some(gp) => gp,
            _ => anyhow::bail!("No group project defined"),
        };
        let configuration = self.projects_data.group_projects_compiler().await;
        let projects = group_project
            .project_links
            .iter()
            .map(|link| {
                self.projects_data
                    .get_project(link.project_id)
                    .ok_or_else(|| anyhow::anyhow!("Project with id {} not found", link.project_id))
            })
            .collect::<Result<Vec<_>>>()?;
        let compiler_name = configuration.product_name.clone();
        return Ok(CompilationParameters {
            projects,
            configuration,
            rebuild,
            only_one_project: false,
            banner: CompBanner::new(
                format!("Compiling Group Project {}", group_project.name),
                format!("Projects of Group Project '{}'", group_project.name),
                compiler_name,
                rebuild,
            ),
        });
    }

    async fn get_from_link_parameters<'a>(
        &'a self,
        project_link_id: usize,
        rebuild: bool,
    ) -> Result<CompilationParameters<'a>> {
        let (projects, configuration, banner);
        if let Some(workspace_id) = self
            .projects_data
            .get_workspace_id_containing_project_link(project_link_id)
        {
            let workspace = self
                .projects_data
                .get_workspace(workspace_id)
                .ok_or_else(|| anyhow::anyhow!("Workspace with id {} not found", workspace_id))?;
            if let Some(index) = workspace.index_of(project_link_id) {
                projects = workspace.project_links[index..]
                    .iter()
                    .map(|link| {
                        self.projects_data
                            .get_project(link.project_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!("Project with id {} not found", link.project_id)
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                configuration = workspace.compiler().await;
                let project_name = projects
                    .first()
                    .map(|p| p.name.clone())
                    .unwrap_or("<unknown>".to_string());
                banner = CompBanner::new(
                    format!("Compiling Workspace '{}' Project {project_name}", workspace.name),
                    format!(
                        "Projects of Workspace '{}' from project {project_name}",
                        workspace.name
                    ),
                    configuration.product_name.clone(),
                    rebuild,
                );
            } else {
                anyhow::bail!(
                    "Project link with id {} not found in workspace {}",
                    project_link_id,
                    workspace.name
                );
            }
        } else if let Some(group_project) = &self.projects_data.group_project {
            if let Some(index) = group_project.index_of(project_link_id) {
                projects = group_project.project_links[index..]
                    .iter()
                    .map(|link| {
                        self.projects_data
                            .get_project(link.project_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!("Project with id {} not found", link.project_id)
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                configuration = self.projects_data.group_projects_compiler().await;
                let project_name = projects
                    .first()
                    .map(|p| p.name.clone())
                    .unwrap_or("<unknown>".to_string());
                banner = CompBanner::new(
                    format!("Compiling Group Project '{}' Project {project_name}", group_project.name),
                    format!(
                        "Projects of Group Project '{}' from project {project_name}",
                        group_project.name
                    ),
                    configuration.product_name.clone(),
                    rebuild,
                );
            } else {
                anyhow::bail!(
                    "Project link with id {} not found in group project {}",
                    project_link_id,
                    group_project.name
                );
            }
        } else {
            anyhow::bail!(
                "No workspace or group project contains project link with id {}",
                project_link_id
            );
        }
        return Ok(CompilationParameters {
            projects,
            configuration,
            rebuild,
            only_one_project: false,
            banner,
        });
    }

    pub async fn compile(&self) -> Result<CompileResult> {
        if !compiler_state::activate() {
            anyhow::bail!(
                "Another compilation is already in progress. Please wait until it finishes."
            );
        }
        defer! {
            compiler_state::reset()
        }

        let parameters = match self.params {
            CompileProjectParams::Project {
                project_id,
                project_link_id,
                rebuild,
                event_id: _
            } => self.get_project_parameters(project_id, project_link_id, rebuild).await?,
            CompileProjectParams::AllInWorkspace {
                workspace_id,
                rebuild,
                event_id: _
            } => self.get_all_workspace_parameters(workspace_id, rebuild).await?,
            CompileProjectParams::AllInGroupProject {
                rebuild,
                event_id: _
            } => self.get_all_group_project_parameters(rebuild).await?,
            CompileProjectParams::FromLink {
                project_link_id,
                rebuild,
                event_id: _,
            } => self.get_from_link_parameters(project_link_id, rebuild).await?,
        };
        clear_stale_diagnostics(self.client.as_ref()).await;
        // Actual compilation process
        let start_lines = if parameters.only_one_project {
            parameters.banner.into_header_vec()
        } else {
            parameters.banner.into_multi_header_vec()
        };
        CompilerProgress::notify_start(
            self.client.as_ref(),
            start_lines
        ).await;
        let result = self.do_compile(&parameters).await;
        let cancelled = compiler_state::is_cancelled();
        // Treat cancellation as a non-error outcome so no upstream error is logged
        let result = if cancelled { Ok(()) } else { result };
        let compile_result = CompileResult {
            success: compiler_state::is_success(),
            cancelled,
            code: compiler_state::get_code(),
        };
        CompilerProgress::notify_completed(
            self.client.as_ref(),
            compile_result.success,
            compile_result.cancelled,
            compile_result.code,
            parameters.banner.into_footer_vec(),
        ).await;
        result?;
        return Ok(compile_result);
    }

    async fn do_compile(&self, parameters: &CompilationParameters<'_>) -> Result<()> {
        for project in &parameters.projects {
            if compiler_state::is_cancelled() {
                return Err(anyhow::anyhow!("Compilation cancelled by user."));
            }

            // Resolve effective configuration/platform for this project early,
            // so the banner can display it and MSBuild receives the right args.
            let (eff_config, eff_platform) = if let Some(dproj_path) = &project.dproj {
                if let Ok(dproj_obj) = dproj_cache::get_or_load(project.id, &PathBuf::from(dproj_path)) {
                    project.effective_config_platform(&dproj_obj)
                } else {
                    (project.active_configuration.clone().unwrap_or_else(|| "Debug".to_string()),
                     project.active_platform.clone().unwrap_or_else(|| "Win32".to_string()))
                }
            } else {
                // Bare `.dpr`/`.dpk`: no dproj defaults exist, so fall back to the
                // synthetic bare-project defaults (matching what `dproj/metadata`
                // advertises to the platform picker).
                (project.active_configuration.clone().unwrap_or_else(|| BARE_DEFAULT_CONFIGURATION.to_string()),
                 project.active_platform.clone().unwrap_or_else(|| BARE_DEFAULT_PLATFORM.to_string()))
            };

            if !parameters.only_one_project {
                CompilerProgress::notify_single_project_started(
                    self.client.as_ref(),
                    project.id,
                    CompBanner::new(
                        format!("Compiling Project: {}", project.name),
                        project.get_project_file()?.to_string_lossy().to_string(),
                        parameters.configuration.product_name.clone(),
                        parameters.rebuild,
                    ).with_config_platform(eff_config.clone(), eff_platform.clone())
                    .into_project_header_vec()
                ).await;
            }

            let rsvars_path = PathBuf::from(&parameters.configuration.installation_path)
                .join("bin")
                .join("rsvars.bat");
            if !rsvars_path.exists() {
                anyhow::bail!(
                    "Cannot find rsvars.bat at path: {}",
                    rsvars_path.to_string_lossy()
                );
            }
            let envs = dproj_rs::rsvars::parse_rsvars_file(&rsvars_path)
                .map_err(|e| anyhow::anyhow!("Failed to parse rsvars.bat: {}", e))?;
            let project_file = project.get_project_file()?;

            // A project with a real `.dproj` is built through MSBuild as before.
            // A bare `.dpr`/`.dpk` (no `.dproj`) cannot be loaded by MSBuild
            // (it is Pascal source, not an MSBuild XML project → MSB4025), so it
            // is compiled with the Delphi command-line compiler directly.
            let has_dproj = project.dproj.as_deref()
                .map(|p| !p.is_empty() && PathBuf::from(p).exists())
                .unwrap_or(false);

            let mut command = if has_dproj {
                let args = parameters.configuration.build_arguments.join(" ");
                let target = if parameters.rebuild { "Build" } else { "Make" };
                let msbuild_path = find_msbuild()?;
                let mut command = Command::new(msbuild_path);
                command
                    .arg(&project_file)
                    .arg(format!("/t:Clean,{}", target))
                    .args(args.split_whitespace())
                    .arg(format!("/p:Config={}", eff_config))
                    .arg(format!("/p:Configuration={}", eff_config))
                    .arg(format!("/p:Platform={}", eff_platform));
                command
            } else {
                let dcc_path = find_dcc(&parameters.configuration.installation_path, &eff_platform)?;
                let mut command = Command::new(dcc_path);
                command
                    .arg(&project_file)
                    .args(dcc_arguments(&eff_config, parameters.rebuild));
                command
            };

            command
                .envs(envs)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            // dcc resolves relative include/output paths against the working
            // directory, so run the compiler from the project's own folder.
            if let Some(dir) = project_file.parent() {
                command.current_dir(dir);
            }
            let mut child_process = command.spawn()?;

            // Capture the PID before taking stdio handles so we can kill the
            // entire process tree on cancellation (taskkill /F /T kills MSBuild
            // AND every compiler child process it spawned, e.g. dcc32.exe).
            let child_pid = child_process.id();

            let stdout = child_process.stdout.take()
                .ok_or_else(|| anyhow::anyhow!("Unable to access child process STDOUT"))?;
            let stderr = child_process.stderr.take()
                .ok_or_else(|| anyhow::anyhow!("Unable to access child process STDERR"))?;

            let out_reader = BufReader::new(stdout);
            let err_reader = BufReader::new(stderr);

            let project_dir = project.get_project_file()?
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(&project.directory));

            let stdout_task = tokio::spawn(process_output_lines(
                self.client.clone(), // Option<tower_lsp::Client>
                out_reader,
                parameters.configuration.product_name.clone(),
                OutputKind::Stdout,
                project_dir.clone(),
            ));

            let stderr_task = tokio::spawn(process_output_lines(
                self.client.clone(), // Option<tower_lsp::Client>
                err_reader,
                parameters.configuration.product_name.clone(),
                OutputKind::Stderr,
                project_dir,
            ));

            let cancel_signal = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    if compiler_state::is_cancelled() {
                        break;
                    }
                }
            };

            let result = tokio::select! {
                status = child_process.wait() => {
                    let status = status?;
                    stdout_task.await?;
                    stderr_task.await?;
                    compiler_state::set_success(status.success());
                    compiler_state::set_code(status.code().unwrap_or(-1));
                    Ok(())
                }
                _ = cancel_signal => {
                    // Kill the whole process tree so that child processes spawned
                    // by MSBuild (dcc32.exe, dcc64.exe, …) are also terminated.
                    // Without this, those processes keep file locks and the next
                    // compilation attempt on the same project fails immediately.
                    if let Some(pid) = child_pid {
                        let _ = std::process::Command::new("taskkill")
                            .args(["/F", "/T", "/PID", &pid.to_string()])
                            .output();
                    }
                    // Fallback: also ask Tokio to kill the root process handle.
                    let _ = child_process.kill().await;
                    stdout_task.abort();
                    stderr_task.abort();
                    compiler_state::set_success(false);
                    compiler_state::set_code(-1);
                    Err(anyhow::anyhow!("Compilation cancelled by user."))
                }
            };

            if !parameters.only_one_project {
                CompilerProgress::notify_single_project_completed(
                    self.client.as_ref(),
                    project.id,
                    compiler_state::is_success(),
                    compiler_state::is_cancelled(),
                    compiler_state::get_code(),
                    CompBanner::new(
                        format!("Compiling Project: {}", project.name),
                        project.get_project_file()?.to_string_lossy().to_string(),
                        parameters.configuration.product_name.clone(),
                        parameters.rebuild,
                    ).with_config_platform(eff_config.clone(), eff_platform.clone())
                    .into_project_footer_vec()
                ).await
            }
            if compiler_state::is_success() {
                let exe_missing = project.exe.as_deref()
                    .map_or(true, |p| p.is_empty() || !PathBuf::from(p).exists());
                if exe_missing {
                    let mut projects_data = PROJECTS_DATA.write().await;
                    if let Some(project) = projects_data.get_project_mut(project.id) &&
                       let Ok(_) = project.discover_paths()
                    {
                        let _ = projects_data.save().await;
                    }
                }
            }
            result?;
        }
        return Ok(());
    }
}

enum OutputKind {
    Stdout,
    Stderr,
}

lazy_static::lazy_static! {
    // Matches Delphi 2007 compiler-progress lines: indented Windows absolute path with no
    // line-number notation, e.g. "  C:\Delphi\VSS\...\SomeUnit".
    static ref PATH_ONLY_LINE_REGEX: regex::Regex =
        regex::Regex::new(r"^\s+[A-Za-z]:\\[^()\r\n]*$").unwrap();
}

async fn process_output_lines<R: AsyncRead + Unpin + Send>(
    client: Option<tower_lsp::Client>,
    mut reader: BufReader<R>,
    compiler_name: String,
    kind: OutputKind,
    project_dir: PathBuf,
) {
    use tokio::io::AsyncBufReadExt;
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut last_file = String::new();
    // Tracks the last emitted (file, line, code) key to deduplicate consecutive identical
    // diagnostics that Delphi 2007 outputs twice (once wrapped in MSBuild format, once plain).
    let mut last_diag_key: Option<(String, u32, String)> = None;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        // Decode using the configured compiler encoding
        let line = crate::encoding::decode_line(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        // Skip blank / whitespace-only lines (Delphi 2007 emits many)
        if line.trim().is_empty() {
            continue;
        }
        // Skip path-only compiler-progress lines, e.g. "  C:\Delphi\VSS\...\SomeUnit"
        if PATH_ONLY_LINE_REGEX.is_match(&line) {
            continue;
        }
        if compiler_state::is_cancelled() {
            break;
        }
        if let Some(mut diagnostic) = CompilerLineDiagnostic::from_line(&line, compiler_name.clone()) {
            // Resolve relative file paths against the project directory
            let file_path = PathBuf::from(&diagnostic.file);
            if file_path.is_relative() {
                let resolved = project_dir.join(&file_path);
                if resolved.exists() {
                    diagnostic.file = resolved.to_string_lossy().to_string();
                }
            }
            // Deduplicate: Delphi 2007 emits the same diagnostic twice – once in the
            // Borland.Delphi.Targets MSBuild wrapper and once as a plain indented line.
            // Skip the second occurrence when it has the same (file, line, code) as the
            // diagnostic we just emitted.
            let key = (diagnostic.file.clone(), diagnostic.line, diagnostic.code.clone());
            if last_diag_key.as_ref() == Some(&key) {
                continue;
            }
            last_diag_key = Some(key);
            if last_file != diagnostic.file && !diagnostics.is_empty() {
                compiler_state::track_diagnosed_file(last_file.clone());
                publish_diagnostics(client.as_ref(), &last_file, &diagnostics).await;
                diagnostics.clear();
            }
            last_file = diagnostic.file.clone();
            let formatted = format!("{}", &diagnostic);
            match kind {
                OutputKind::Stdout => CompilerProgress::notify_stdout(client.as_ref(), formatted).await,
                OutputKind::Stderr => CompilerProgress::notify_stderr(client.as_ref(), formatted).await,
            }
            diagnostics.push(diagnostic.into());
            continue;
        }
        match kind {
            OutputKind::Stdout => CompilerProgress::notify_stdout(client.as_ref(), line).await,
            OutputKind::Stderr => CompilerProgress::notify_stderr(client.as_ref(), line).await,
        }
    }

    if !diagnostics.is_empty() {
        compiler_state::track_diagnosed_file(last_file.clone());
        publish_diagnostics(client.as_ref(), &last_file, &diagnostics).await;
    }
}

/// Resolve the Delphi command-line compiler for a bare `.dpr`/`.dpk` build.
/// The platform selects the bitness: `Win64` → `dcc64.exe`, everything else
/// (including `Win32`) → `dcc32.exe`. The binary lives in the compiler's
/// `bin` directory.
fn find_dcc(installation_path: &str, platform: &str) -> Result<PathBuf> {
    let exe = if platform.eq_ignore_ascii_case("Win64") {
        "dcc64.exe"
    } else {
        "dcc32.exe"
    };
    let path = PathBuf::from(installation_path).join("bin").join(exe);
    if path.exists() {
        return Ok(path);
    }
    anyhow::bail!(
        "Cannot find {} at path: {}. Please ensure the Delphi installation path is correct.",
        exe,
        path.to_string_lossy()
    );
}

/// Build the dcc command-line switches for a bare-source compile. There is no
/// `.dproj` to carry configuration, so the (Debug|Release) selection is mapped
/// onto the relevant `-$` compiler directives. `rebuild` adds `-B` to force a
/// full build of every unit.
fn dcc_arguments(config: &str, rebuild: bool) -> Vec<String> {
    let mut args = vec!["-Q".to_string()]; // quiet: suppress per-unit progress chatter
    if rebuild {
        args.push("-B".to_string()); // build all units, not just out-of-date ones
    }
    if config.eq_ignore_ascii_case("Release") {
        args.push("-$O+".to_string()); // optimization on
        args.push("-$D-".to_string()); // no debug information
        args.push("-$L-".to_string()); // no local symbols
        args.push("-$Y-".to_string()); // no symbol reference info
    } else {
        // Debug (also the fallback for any unknown configuration name)
        args.push("-$O-".to_string()); // optimization off
        args.push("-$D+".to_string()); // debug information
        args.push("-$L+".to_string()); // local symbols
        args.push("-$Y+".to_string()); // symbol reference info
        args.push("-V".to_string());   // emit TD32 debug info for the debugger
    }
    args
}

fn find_msbuild() -> Result<String> {
    let mut search: Vec<String> = SearchBuilder::default()
        .location(r"C:\Windows\Microsoft.NET\Framework\")
        .search_input("msbuild.exe")
        .depth(2)
        .ignore_case()
        .build()
        .collect();
    search.retain(|path| path.to_lowercase().ends_with("msbuild.exe"));
    search.sort_by(
        |left, right| {
            let left_version = left
                .split('\\')
                .rev()
                .nth(1)
                .unwrap_or("v0");
            let right_version = right
                .split('\\')
                .rev()
                .nth(1)
                .unwrap_or("v0");
            right_version.cmp(left_version)
        });
    if let Some(msbuild_path) = search.first() {
        return Ok(msbuild_path.clone());
    }
    anyhow::bail!(
        "Cannot find msbuild.exe in C:\\Windows\\Microsoft.NET\\Framework\\. Please ensure that MSBuild is installed and try again."
    );
}



async fn clear_stale_diagnostics(client: Option<&tower_lsp::Client>) {
    // Always drain the tracked files to prevent stale state
    let files = compiler_state::take_diagnosed_files();
    let Some(client) = client else { return };
    let mut tasks = tokio::task::JoinSet::new();
    for file in files {
        let client = client.clone();
        tasks.spawn(async move {
            publish_diagnostics(Some(&client), &file, &vec![]).await;
        });
    }
    while tasks.join_next().await.is_some() {}
}

async fn publish_diagnostics(
    client: Option<&tower_lsp::Client>,
    file: &str,
    diagnostics: &Vec<Diagnostic>,
) {
    let Some(client) = client else { return };
    let uri = Url::from_file_path(file).unwrap_or_else(|_| Url::parse("untitled:unknown").unwrap());
    client
        .publish_diagnostics(uri, diagnostics.clone(), None)
        .await;
}

fn format_line(text: &str, total_width: usize) -> String {
    let padding = total_width.saturating_sub(text.len() + 2);
    if padding == 0 {
        return text.to_string();
    }
    let left_padding = padding / 2;
    format!(" {}{}", " ".repeat(left_padding), text)
}

#[derive(Debug, Clone)]
struct CompilationParameters<'compiler> {
    projects: Vec<&'compiler Project>,
    configuration: CompilerConfiguration,
    rebuild: bool,
    only_one_project: bool,
    banner: CompBanner,
}

unsafe impl Send for CompilationParameters<'_> {}
unsafe impl Sync for CompilationParameters<'_> {}

const BANNER_TOP: &str               = "╒══════════════════════════════════════════════════════════════════════╕";
const BANNER_BOTTOM: &str            = "╘══════════════════════════════════════════════════════════════════════╛";
const BANNER_ERROR_TOP: &str         = "╔══════════════════════════════════════════════════════════════════════╗";
const BANNER_ERROR_BOTTOM: &str      = "╚══════════════════════════════════════════════════════════════════════╝";
const BANNER_SUCCESS_TOP: &str       = "┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓";
const BANNER_SUCCESS_BOTTOM: &str    = "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛";
const BANNER_CANCELLED_TOP: &str     = "╓──────────────────────────────────────────────────────────────────────╖";
const BANNER_CANCELLED_BOTTOM: &str  = "╙──────────────────────────────────────────────────────────────────────╜";
const BANNER_MULTI_TOP: &str         = "┍━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┑";
const BANNER_MULTI_BOTTOM: &str      = "┕━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┙";
const BANNER_PROJECT_TOP: &str       = "┌──────────────────────────────────────────────────────────────────────┐";
const BANNER_PROJECT_BOTTOM: &str    = "└──────────────────────────────────────────────────────────────────────┘";

#[derive(Debug, Clone)]
struct CompBanner {
    title: String,
    target: String,
    compiler_name: String,
    rebuild: bool,
    /// Optional per-project config/platform shown in the banner.
    config_platform: Option<(String, String)>,
}

impl CompBanner {
    fn new(title: String, target: String, compiler_name: String, rebuild: bool) -> Self {
        CompBanner {
            title,
            target,
            compiler_name,
            rebuild,
            config_platform: None,
        }
    }

    fn with_config_platform(mut self, config: String, platform: String) -> Self {
        self.config_platform = Some((config, platform));
        self
    }

    fn action_str(&self) -> &str {
        if self.rebuild {
            "Rebuild (Clean;Build)"
        } else {
            "Compile (Clean;Make)"
        }
    }

    fn base_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format_line(&self.title, 72),
            format_line(&format!("→ {} ←", self.target), 70),
            format_line(&format!("🛠️ Compiler: {}", self.compiler_name), 70),
        ];
        if let Some((config, platform)) = &self.config_platform {
            lines.push(format_line(&format!("📋 Config: {} | Platform: {}", config, platform), 70));
        }
        lines.push(format_line(&format!("🗲 Action: {}", self.action_str()), 70));
        lines
    }

    fn into_header_vec(&self) -> Vec<String> {
        let mut lines = vec![BANNER_TOP.to_string()];
        lines.extend(self.base_lines());
        lines.push(BANNER_BOTTOM.to_string());
        lines
    }

    fn into_multi_header_vec(&self) -> Vec<String> {
        let mut lines = vec![BANNER_MULTI_TOP.to_string()];
        lines.extend(self.base_lines());
        lines.push(BANNER_MULTI_BOTTOM.to_string());
        lines
    }

    fn into_project_header_vec(&self) -> Vec<String> {
        let mut lines = vec![BANNER_PROJECT_TOP.to_string()];
        lines.extend(self.base_lines());
        lines.push(BANNER_PROJECT_BOTTOM.to_string());
        lines
    }

    fn into_project_footer_vec(&self) -> Vec<String> {
        let cancelled = compiler_state::is_cancelled();
        let success = compiler_state::is_success();
        let status_str = if cancelled {
            "⚠️  CANCELLED"
        } else if success {
            "✅ SUCCESS"
        } else {
            "❌ FAILED"
        };
        let mut lines = vec![BANNER_PROJECT_TOP.to_string()];
        lines.extend(self.base_lines());
        lines.push(format_line(&format!("Status: {}", status_str), 70));
        lines.push(BANNER_PROJECT_BOTTOM.to_string());
        lines
    }

    fn into_footer_vec(&self) -> Vec<String> {
        let cancelled = compiler_state::is_cancelled();
        let success = compiler_state::is_success();
        let (status_str, top, bottom) = if cancelled {
            ("⚠️  CANCELLED", BANNER_CANCELLED_TOP, BANNER_CANCELLED_BOTTOM)
        } else if success {
            ("✅ SUCCESS", BANNER_SUCCESS_TOP, BANNER_SUCCESS_BOTTOM)
        } else {
            ("❌ FAILED", BANNER_ERROR_TOP, BANNER_ERROR_BOTTOM)
        };
        let mut lines = vec![top.to_string()];
        lines.extend(self.base_lines());
        lines.push(format_line(&format!("Status: {}", status_str), 70));
        lines.push(bottom.to_string());
        lines
    }
}
