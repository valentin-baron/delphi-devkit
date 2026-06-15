//! Shared command implementations for DDK.
//!
//! Both the MCP server and the CLI binary delegate to these functions.
//! Each function returns a typed Rust struct; the caller decides how to
//! present it (JSON for MCP, human-readable table for CLI, etc.).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::lsp_types::{CompileProjectParams, CompilerProgress, CompilerProgressParams};
use crate::projects::*;
use crate::state::*;
use crate::utils::normalize_path;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Summary of a single project entry within a workspace or group project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: usize,
    pub name: String,
    pub directory: String,
    pub dproj: Option<String>,
    pub exe: Option<String>,
    pub active: bool,
}

/// Summary of a user-defined workspace and its projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: usize,
    pub name: String,
    pub compiler_id: String,
    pub projects: Vec<ProjectSummary>,
}

/// Summary of the loaded group project and its projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupProjectSummary {
    pub name: String,
    pub path: String,
    pub compiler_id: String,
    pub projects: Vec<ProjectSummary>,
}

/// Hierarchical project listing preserving workspace / group-project structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectListResult {
    pub workspaces: Vec<WorkspaceSummary>,
    pub group_project: Option<GroupProjectSummary>,
    pub active_project_id: Option<usize>,
}

impl fmt::Display for ProjectListResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.workspaces.is_empty() && self.group_project.is_none() {
            return write!(f, "No projects found.");
        }

        for ws in &self.workspaces {
            writeln!(f, "Workspace: {} (compiler: {})", ws.name, ws.compiler_id)?;
            if ws.projects.is_empty() {
                writeln!(f, "  (empty)")?;
            } else {
                for p in &ws.projects {
                    let marker = if p.active { " *" } else { "" };
                    writeln!(f, "  [{}]{} {} ({})", p.id, marker, p.name, p.directory)?;
                    if let Some(exe) = &p.exe {
                        writeln!(f, "       exe: {exe}")?;
                    }
                }
            }
        }

        if let Some(gp) = &self.group_project {
            writeln!(f, "Group Project: {} (compiler: {})", gp.name, gp.compiler_id)?;
            if gp.projects.is_empty() {
                writeln!(f, "  (empty)")?;
            } else {
                for p in &gp.projects {
                    let marker = if p.active { " *" } else { "" };
                    writeln!(f, "  [{}]{} {} ({})", p.id, marker, p.name, p.directory)?;
                    if let Some(exe) = &p.exe {
                        writeln!(f, "       exe: {exe}")?;
                    }
                }
            }
        }

        Ok(())
    }
}

/// Environment info for the currently active project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub project: Option<EnvironmentProject>,
    pub group_project_compiler: Option<CompilerSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentProject {
    pub id: usize,
    pub name: String,
    pub directory: String,
    pub dproj: Option<String>,
    pub compilers: Vec<EnvironmentCompilerEntry>,
}

/// A compiler associated with a specific context (workspace name or
/// "group_project").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentCompilerEntry {
    pub context: String,
    pub key: String,
    pub product_name: String,
    pub product_version: usize,
    pub compiler_version: usize,
    pub installation_path: String,
}

impl fmt::Display for EnvironmentInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.project {
            Some(proj) => {
                writeln!(f, "Active project: {} (ID {})", proj.name, proj.id)?;
                writeln!(f, "  Directory: {}", proj.directory)?;
                if let Some(dproj) = &proj.dproj {
                    writeln!(f, "  Dproj:     {dproj}")?;
                }
                if !proj.compilers.is_empty() {
                    writeln!(f, "  Compilers:")?;
                    for entry in &proj.compilers {
                        writeln!(
                            f,
                            "    [{context}] {name} v{ver} ({key})",
                            context = entry.context,
                            name = entry.product_name,
                            ver = entry.product_version,
                            key = entry.key,
                        )?;
                    }
                }
            }
            _ => {
                writeln!(f, "No active project.")?;
            }
        }
        if let Some(gc) = &self.group_project_compiler {
            writeln!(
                f,
                "Group project compiler: {} ({}) at {}",
                gc.product_name, gc.key, gc.installation_path
            )?;
        }
        Ok(())
    }
}

/// Summary of a compiler configuration (returned by `list_compilers`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerSummary {
    pub key: String,
    pub product_name: String,
    pub product_version: usize,
    pub compiler_version: usize,
    pub installation_path: String,
}

impl fmt::Display for CompilerSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{key}  {name} v{ver}  ({path})",
            key = self.key,
            name = self.product_name,
            ver = self.product_version,
            path = self.installation_path,
        )
    }
}

/// Confirmation after selecting a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectProjectResult {
    pub project_id: usize,
    pub project_name: String,
}

impl fmt::Display for SelectProjectResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Selected project: {} (ID {}).",
            self.project_name, self.project_id
        )
    }
}

/// Confirmation after setting the group project compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetCompilerResult {
    pub key: String,
    pub product_name: String,
}

impl fmt::Display for SetCompilerResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Group project compiler set to: {} ({}).",
            self.product_name, self.key
        )
    }
}

/// Full compilation output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileOutput {
    pub project_name: String,
    pub success: bool,
    pub cancelled: bool,
    pub code: i32,
    pub lines: Vec<String>,
}

pub type CompileProgressCallback = std::sync::Arc<dyn Fn(String) + Send + Sync>;

/// Output filter options for `cmd_compile` / `cmd_compile_with_progress`.
///
/// The VS Code extension consumes compiler events directly via the LSP server
/// (which never goes through these commands), so its output remains untouched.
/// CLI and MCP callers should set these to reduce token noise.
#[derive(Debug, Clone, Default)]
pub struct CompileFilterOptions {
    /// Strip box-drawing border lines from start/completed banners and trim
    /// the centered padding on the remaining info lines.
    pub trim_banners: bool,
    /// Emit warning lines verbatim. When false, warnings are hidden (and
    /// optionally counted toward `summarize_diagnostics`).
    pub show_warnings: bool,
    /// Emit hint lines verbatim. When false, hints are hidden (and optionally
    /// counted toward `summarize_diagnostics`).
    pub show_hints: bool,
    /// Emit a per-file summary `<file>: X warn, Y hint` after each project's
    /// completion event for any diagnostics that were not shown verbatim.
    pub summarize_diagnostics: bool,
}

impl fmt::Display for CompileOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = if self.cancelled {
            format!("Compilation of \"{}\" was cancelled.", self.project_name)
        } else if self.success {
            format!(
                "Project \"{}\" compiled successfully.",
                self.project_name
            )
        } else {
            format!(
                "Compilation of \"{}\" finished with errors (exit code {}).",
                self.project_name, self.code
            )
        };
        write!(f, "{summary}")?;
        if !self.lines.is_empty() {
            write!(f, "\n\nCompiler output:\n{}", self.lines.join("\n"))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output-filter helpers (used by cmd_compile_with_progress)
// ---------------------------------------------------------------------------

/// Returns `true` if `line` is a banner border (only box-drawing chars and
/// whitespace). These are the decorative top/bottom rows of the compile
/// banner that have no value for an LLM consumer.
fn is_banner_border_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.chars().all(|c| matches!(
        c,
        '╒' | '╕' | '╘' | '╛'
        | '╔' | '╗' | '╚' | '╝'
        | '┏' | '┓' | '┗' | '┛'
        | '╓' | '╖' | '╙' | '╜'
        | '┍' | '┑' | '┕' | '┙'
        | '┌' | '┐' | '└' | '┘'
        | '─' | '━' | '═' | ' '
    ))
}

/// Trim a banner line vector for compact CLI/MCP output: drop border rows and
/// strip the centering padding from the remaining info rows.
fn trim_banner_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|l| !is_banner_border_line(l))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

lazy_static::lazy_static! {
    /// Matches the formatted-diagnostic line emitted by
    /// `CompilerLineDiagnostic::Display`:
    ///   `HH:MM:SS.mmm: [KIND][CODE] file:line[:col] - message`
    static ref FORMATTED_DIAG_REGEX: regex::Regex = regex::Regex::new(
        r"^\d{2}:\d{2}:\d{2}\.\d+:\s+\[(?P<kind>WARN|HINT|ERROR)\]\[[A-Z]\d+\]\s+(?P<file>.+?):\d+(?::\d+)?\s+-\s"
    ).unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagKind {
    Warn,
    Hint,
    Error,
}

/// Attempt to classify a streamed compiler line as a formatted diagnostic.
/// Returns `(kind, file_basename_without_extension)` on match.
fn classify_diagnostic_line(line: &str) -> Option<(DiagKind, String)> {
    let caps = FORMATTED_DIAG_REGEX.captures(line)?;
    let kind = match caps.name("kind")?.as_str() {
        "WARN" => DiagKind::Warn,
        "HINT" => DiagKind::Hint,
        "ERROR" => DiagKind::Error,
        _ => return None,
    };
    let file = caps.name("file")?.as_str();
    Some((kind, diag_file_basename(file)))
}

/// Extract `<filename without extension>` from a path string.
/// Handles both `/` and `\` separators since Delphi runs on Windows.
fn diag_file_basename(path: &str) -> String {
    let last = path.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(path);
    match last.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => last.to_string(),
    }
}

/// Per-project tracker for warnings/hints suppressed from streamed output.
/// Counts are aggregated by file basename in insertion order so the summary
/// reflects the order diagnostics arrived.
#[derive(Debug, Default)]
struct DiagCounts {
    order: Vec<String>,
    counts: std::collections::HashMap<String, (u32, u32)>,
}

impl DiagCounts {
    fn add(&mut self, file: &str, kind: DiagKind) {
        let entry = self.counts.entry(file.to_string()).or_insert_with(|| {
            self.order.push(file.to_string());
            (0, 0)
        });
        match kind {
            DiagKind::Warn => entry.0 += 1,
            DiagKind::Hint => entry.1 += 1,
            DiagKind::Error => {}
        }
    }

    fn drain_summary_lines(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for file in self.order.drain(..) {
            if let Some((w, h)) = self.counts.remove(&file) {
                if w == 0 && h == 0 {
                    continue;
                }
                out.push(format!("{file}: {w} warn, {h} hint"));
            }
        }
        self.counts.clear();
        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the first `ProjectLink.id` for a given project, searching workspaces
/// first, then the group project.
pub fn find_project_link_id(data: &ProjectsData, project_id: usize) -> Option<usize> {
    for ws in &data.workspaces {
        if let Some(link) = ws.project_links.iter().find(|l| l.project_id == project_id) {
            return Some(link.id);
        }
    }
    if let Some(gp) = &data.group_project {
        if let Some(link) = gp.project_links.iter().find(|l| l.project_id == project_id) {
            return Some(link.id);
        }
    }
    None
}

/// A project candidate surfaced when a name reference is ambiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: usize,
    pub name: String,
    /// Workspace or group-project name the project belongs to (or "(unlinked)").
    pub location: String,
    /// The project's primary file (.dproj/.dpr/.dpk) or its directory.
    pub path: String,
}

/// Outcome of resolving a project reference (name or numeric id).
#[derive(Debug, Clone)]
pub enum ProjectResolution {
    /// Exactly one project matched; carries its id.
    Single(usize),
    /// Several projects matched; the caller should present these candidates.
    Ambiguous(Vec<ProjectRef>),
    /// Nothing matched.
    NotFound,
}

/// The workspace/group-project name that contains `project_id`.
fn project_location(data: &ProjectsData, project_id: usize) -> String {
    for ws in &data.workspaces {
        if ws.project_links.iter().any(|l| l.project_id == project_id) {
            return ws.name.clone();
        }
    }
    if let Some(gp) = &data.group_project {
        if gp.project_links.iter().any(|l| l.project_id == project_id) {
            return gp.name.clone();
        }
    }
    "(unlinked)".to_string()
}

fn project_ref(data: &ProjectsData, p: &Project) -> ProjectRef {
    let path = p
        .dproj
        .clone()
        .or_else(|| p.dpr.clone())
        .or_else(|| p.dpk.clone())
        .unwrap_or_else(|| p.directory.clone());
    ProjectRef {
        id: p.id,
        name: p.name.clone(),
        location: project_location(data, p.id),
        path,
    }
}

/// Resolve a project reference to a concrete project.
///
/// A reference that parses as a number and matches an existing project id wins
/// outright. Otherwise the reference is matched against project names: an
/// exact (case-insensitive) match is preferred, falling back to a
/// case-insensitive substring match. A single match resolves to that project;
/// multiple matches are returned as candidates so the caller can disambiguate.
pub fn resolve_project_reference(data: &ProjectsData, reference: &str) -> ProjectResolution {
    if let Ok(id) = reference.parse::<usize>() {
        if data.get_project(id).is_some() {
            return ProjectResolution::Single(id);
        }
    }
    let needle = reference.to_lowercase();
    let exact: Vec<&Project> = data
        .projects
        .iter()
        .filter(|p| p.name.to_lowercase() == needle)
        .collect();
    let chosen: Vec<&Project> = if exact.is_empty() {
        data.projects
            .iter()
            .filter(|p| p.name.to_lowercase().contains(&needle))
            .collect()
    } else {
        exact
    };
    match chosen.len() {
        0 => ProjectResolution::NotFound,
        1 => ProjectResolution::Single(chosen[0].id),
        _ => ProjectResolution::Ambiguous(chosen.iter().map(|p| project_ref(data, p)).collect()),
    }
}

/// Resolve a project **file path** to the managed project(s) that own it
/// (i.e. whose `.dproj`/`.dpr`/`.dpk` is that file). Paths are normalised and
/// compared case-insensitively (Windows). Used so that `compile <path>` for a
/// file already belonging to a project behaves like referencing that project
/// by name, rather than compiling it ad-hoc.
pub fn resolve_project_by_path(data: &ProjectsData, path: &str) -> ProjectResolution {
    let target = normalize_path(path).to_string_lossy().to_lowercase();
    let owns = |field: &Option<String>| -> bool {
        field
            .as_ref()
            .map(|p| normalize_path(p).to_string_lossy().to_lowercase() == target)
            .unwrap_or(false)
    };
    let chosen: Vec<&Project> = data
        .projects
        .iter()
        .filter(|p| owns(&p.dproj) || owns(&p.dpr) || owns(&p.dpk))
        .collect();
    match chosen.len() {
        0 => ProjectResolution::NotFound,
        1 => ProjectResolution::Single(chosen[0].id),
        _ => ProjectResolution::Ambiguous(chosen.iter().map(|p| project_ref(data, p)).collect()),
    }
}

/// Resolve a user-supplied compiler reference to a concrete configuration key.
///
/// Matching order: exact key (`"12.0"`) → exact product name (case-insensitive,
/// e.g. `"Delphi 12.0 Athens"`) → unique product-name substring (e.g.
/// `"Delphi 12"` or `"Athens"`). When `requested` is `None`, the newest
/// installed compiler (highest `compiler_version`, preferring `"12.0"` on a
/// tie) is chosen. Errors list the available compilers.
async fn resolve_compiler_key(requested: Option<String>) -> Result<String> {
    let configs = COMPILER_CONFIGURATIONS.read().await;
    if configs.iter().next().is_none() {
        bail!("No compiler configurations available.");
    }
    let available = || -> String {
        let mut entries: Vec<String> = configs
            .iter()
            .map(|(k, c)| format!("{k} ({})", c.product_name))
            .collect();
        entries.sort();
        entries.join(", ")
    };

    let Some(req) = requested else {
        // No preference: prefer the canonical default, else the newest compiler.
        if configs.contains_key("12.0") {
            return Ok("12.0".to_string());
        }
        return configs
            .iter()
            .max_by_key(|(_, c)| c.compiler_version)
            .map(|(k, _)| k.clone())
            .ok_or_else(|| anyhow::anyhow!("No compiler configurations available."));
    };

    if configs.contains_key(&req) {
        return Ok(req);
    }
    let needle = req.to_lowercase();
    let exact: Vec<String> = configs
        .iter()
        .filter(|(_, c)| c.product_name.to_lowercase() == needle)
        .map(|(k, _)| k.clone())
        .collect();
    if exact.len() == 1 {
        return Ok(exact.into_iter().next().unwrap());
    }
    let partial: Vec<String> = configs
        .iter()
        .filter(|(_, c)| c.product_name.to_lowercase().contains(&needle))
        .map(|(k, _)| k.clone())
        .collect();
    match partial.len() {
        1 => Ok(partial.into_iter().next().unwrap()),
        0 => bail!("Unknown compiler \"{req}\". Available: {}", available()),
        _ => bail!(
            "Ambiguous compiler \"{req}\" matches keys: {}. Use an exact key from: {}",
            partial.join(", "),
            available()
        ),
    }
}

/// Resolve a workspace reference (name, or numeric id) to a workspace id.
/// Names are matched exactly first, then case-insensitively. Errors list the
/// available workspace names.
pub fn resolve_workspace_id(data: &ProjectsData, reference: &str) -> Result<usize> {
    if let Some(ws) = data.workspaces.iter().find(|w| w.name == reference) {
        return Ok(ws.id);
    }
    let needle = reference.to_lowercase();
    let ci: Vec<&Workspace> = data
        .workspaces
        .iter()
        .filter(|w| w.name.to_lowercase() == needle)
        .collect();
    if ci.len() == 1 {
        return Ok(ci[0].id);
    }
    if let Ok(id) = reference.parse::<usize>() {
        if data.workspaces.iter().any(|w| w.id == id) {
            return Ok(id);
        }
    }
    let names: Vec<String> = data.workspaces.iter().map(|w| format!("\"{}\"", w.name)).collect();
    if names.is_empty() {
        bail!("No workspaces exist yet. Create one first (e.g. `ddk projects add_workspace`).");
    }
    bail!("Workspace \"{reference}\" not found. Available: {}", names.join(", "));
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Returns environment information for the currently active project.
pub async fn cmd_get_environment_info() -> Result<EnvironmentInfo> {
    let projects_data = PROJECTS_DATA.read().await;
    let compilers = COMPILER_CONFIGURATIONS.read().await;

    let project = match projects_data.active_project_id {
        Some(id) => projects_data.get_project(id),
        _ => None,
    };

    let env_project = project.map(|p| {
        let mut entries = Vec::new();

        for workspace in &projects_data.workspaces {
            for link in &workspace.project_links {
                if link.project_id == p.id {
                    if let Some(compiler) = compilers.get(&workspace.compiler_id) {
                        entries.push(EnvironmentCompilerEntry {
                            context: workspace.name.clone(),
                            key: workspace.compiler_id.clone(),
                            product_name: compiler.product_name.clone(),
                            product_version: compiler.product_version,
                            compiler_version: compiler.compiler_version,
                            installation_path: compiler.installation_path.clone(),
                        });
                    }
                }
            }
        }

        if let Some(group_project) = &projects_data.group_project {
            for link in &group_project.project_links {
                if link.project_id == p.id {
                    if let Some(compiler) =
                        compilers.get(&projects_data.group_project_compiler_id)
                    {
                        entries.push(EnvironmentCompilerEntry {
                            context: "group_project".to_string(),
                            key: projects_data.group_project_compiler_id.clone(),
                            product_name: compiler.product_name.clone(),
                            product_version: compiler.product_version,
                            compiler_version: compiler.compiler_version,
                            installation_path: compiler.installation_path.clone(),
                        });
                    }
                }
            }
        }

        EnvironmentProject {
            id: p.id,
            name: p.name.clone(),
            directory: p.directory.clone(),
            dproj: p.dproj.clone(),
            compilers: entries,
        }
    });

    let group_project_compiler = compilers
        .get(&projects_data.group_project_compiler_id)
        .map(|c| CompilerSummary {
            key: projects_data.group_project_compiler_id.clone(),
            product_name: c.product_name.clone(),
            product_version: c.product_version,
            compiler_version: c.compiler_version,
            installation_path: c.installation_path.clone(),
        });

    Ok(EnvironmentInfo {
        project: env_project,
        group_project_compiler,
    })
}

/// Lists all known projects, preserving workspace / group-project hierarchy.
pub async fn cmd_list_projects() -> Result<ProjectListResult> {
    let projects_data = PROJECTS_DATA.read().await;
    let active_id = projects_data.active_project_id;

    let make_summary = |p: &crate::projects::Project| ProjectSummary {
        id: p.id,
        name: p.name.clone(),
        directory: p.directory.clone(),
        dproj: p.dproj.clone(),
        exe: p.exe.clone(),
        active: Some(p.id) == active_id,
    };

    let workspaces = projects_data
        .workspaces
        .iter()
        .map(|ws| {
            let projects = ws
                .project_links
                .iter()
                .filter_map(|link| {
                    projects_data
                        .get_project(link.project_id)
                        .map(&make_summary)
                })
                .collect();
            WorkspaceSummary {
                id: ws.id,
                name: ws.name.clone(),
                compiler_id: ws.compiler_id.clone(),
                projects,
            }
        })
        .collect();

    let group_project = projects_data.group_project.as_ref().map(|gp| {
        let projects = gp
            .project_links
            .iter()
            .filter_map(|link| {
                projects_data
                    .get_project(link.project_id)
                    .map(&make_summary)
            })
            .collect();
        GroupProjectSummary {
            name: gp.name.clone(),
            path: gp.path.clone(),
            compiler_id: projects_data.group_project_compiler_id.clone(),
            projects,
        }
    });

    Ok(ProjectListResult {
        workspaces,
        group_project,
        active_project_id: active_id,
    })
}

/// Selects a project by ID.
pub async fn cmd_select_project(project_id: usize) -> Result<SelectProjectResult> {
    {
        let data = PROJECTS_DATA.read().await;
        if data.get_project(project_id).is_none() {
            bail!("No project found with ID {project_id}.");
        }
    }

    let change = Change::SelectProject { project_id };
    change.execute().await?;

    let data = PROJECTS_DATA.read().await;
    let name = data
        .get_project(project_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("ID {project_id}"));

    Ok(SelectProjectResult {
        project_id,
        project_name: name,
    })
}

/// Lists all available compiler configurations.
pub async fn cmd_list_compilers() -> Result<Vec<CompilerSummary>> {
    let configs = COMPILER_CONFIGURATIONS.read().await;
    Ok(configs
        .iter()
        .map(|(key, cfg)| CompilerSummary {
            key: key.clone(),
            product_name: cfg.product_name.clone(),
            product_version: cfg.product_version,
            compiler_version: cfg.compiler_version,
            installation_path: cfg.installation_path.clone(),
        })
        .collect())
}

/// Sets the group project compiler by key.
pub async fn cmd_set_group_compiler(compiler_key: String) -> Result<SetCompilerResult> {
    {
        let configs = COMPILER_CONFIGURATIONS.read().await;
        if !configs.contains_key(&compiler_key) {
            let available: Vec<String> = configs.keys().cloned().collect();
            bail!(
                "Unknown compiler key: \"{compiler_key}\". Available keys: {}",
                available.join(", ")
            );
        }
    }

    let change = Change::SetGroupProjectCompiler {
        compiler: compiler_key.clone(),
    };
    change.execute().await?;

    let configs = COMPILER_CONFIGURATIONS.read().await;
    let name = configs
        .get(&compiler_key)
        .map(|c| c.product_name.clone())
        .unwrap_or_default();

    Ok(SetCompilerResult {
        key: compiler_key,
        product_name: name,
    })
}

/// Confirmation after adding a project to a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProjectResult {
    pub project_id: usize,
    pub project_name: String,
    pub workspace_id: usize,
    pub workspace_name: String,
    pub dproj: Option<String>,
    pub dpr: Option<String>,
    pub dpk: Option<String>,
    pub exe: Option<String>,
}

impl fmt::Display for AddProjectResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Added project \"{}\" (ID {}) to workspace \"{}\".",
            self.project_name, self.project_id, self.workspace_name
        )
    }
}

/// Confirmation after creating a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddWorkspaceResult {
    pub workspace_id: usize,
    pub name: String,
    pub compiler_key: String,
    pub compiler_product_name: String,
}

impl fmt::Display for AddWorkspaceResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Created workspace \"{}\" (ID {}) with compiler {} ({}).",
            self.name, self.workspace_id, self.compiler_product_name, self.compiler_key
        )
    }
}

/// Adds a project to an existing workspace, identified by workspace name (or
/// numeric id). The file may be a `.dproj`, `.dpr`, or `.dpk`; bare sources
/// without a `.dproj` are supported. Returns the newly created project.
pub async fn cmd_add_project(file_path: String, workspace: String) -> Result<AddProjectResult> {
    if !std::path::Path::new(&file_path).exists() {
        bail!("File not found: {file_path}");
    }
    let workspace_id = {
        let data = PROJECTS_DATA.read().await;
        resolve_workspace_id(&data, &workspace)?
    };

    let change = Change::NewProject {
        file_path: file_path.clone(),
        workspace_id,
    };
    change.execute().await?;

    // The newly added project is the last link in the target workspace.
    let data = PROJECTS_DATA.read().await;
    let ws = data
        .get_workspace(workspace_id)
        .ok_or_else(|| anyhow::anyhow!("Workspace with id {workspace_id} disappeared after add."))?;
    let workspace_name = ws.name.clone();
    let project = ws
        .project_links
        .last()
        .and_then(|link| data.get_project(link.project_id))
        .ok_or_else(|| anyhow::anyhow!("Project was not added to workspace \"{workspace_name}\"."))?;

    Ok(AddProjectResult {
        project_id: project.id,
        project_name: project.name.clone(),
        workspace_id,
        workspace_name,
        dproj: project.dproj.clone(),
        dpr: project.dpr.clone(),
        dpk: project.dpk.clone(),
        exe: project.exe.clone(),
    })
}

/// Creates a new workspace bound to a compiler configuration. `compiler` is
/// resolved like the compile commands: exact key, exact product name, or a
/// unique product-name substring (e.g. `"Delphi 12"`).
pub async fn cmd_add_workspace(name: String, compiler: String) -> Result<AddWorkspaceResult> {
    if name.trim().is_empty() {
        bail!("Workspace name cannot be empty.");
    }
    {
        let data = PROJECTS_DATA.read().await;
        if data.workspaces.iter().any(|w| w.name == name) {
            bail!("A workspace named \"{name}\" already exists.");
        }
    }
    let compiler_key = resolve_compiler_key(Some(compiler)).await?;
    let compiler_product_name = {
        let configs = COMPILER_CONFIGURATIONS.read().await;
        configs.get(&compiler_key).map(|c| c.product_name.clone()).unwrap_or_default()
    };

    let change = Change::AddWorkspace {
        name: name.clone(),
        compiler: compiler_key.clone(),
    };
    change.execute().await?;

    let data = PROJECTS_DATA.read().await;
    let workspace_id = data
        .workspaces
        .iter()
        .find(|w| w.name == name)
        .map(|w| w.id)
        .ok_or_else(|| anyhow::anyhow!("Workspace \"{name}\" was not created."))?;

    Ok(AddWorkspaceResult {
        workspace_id,
        name,
        compiler_key,
        compiler_product_name,
    })
}

/// Result of formatting a file in-place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatFileResult {
    pub file_path: String,
}

impl fmt::Display for FormatFileResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Formatted: {}", self.file_path)
    }
}

/// Several projects matched a name reference; presented to the user instead of
/// compiling so they can re-run with a specific project id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbiguousProjects {
    pub reference: String,
    pub matches: Vec<ProjectRef>,
}

impl fmt::Display for AmbiguousProjects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Project \"{}\" matches multiple projects:", self.reference)?;
        for m in &self.matches {
            writeln!(f, "- ID {} = {} - {} ({})", m.id, m.location, m.name, m.path)?;
        }
        write!(f, "Re-run targeting the specific project ID to compile the correct one.")
    }
}

/// Result of a reference-based compile: either the compilation output, or a
/// list of candidate projects when the reference was ambiguous.
#[derive(Debug, Clone)]
pub enum CompileOrAmbiguity {
    Output(CompileOutput),
    Ambiguity(AmbiguousProjects),
}

/// Compiles a project. If `project_id` is `Some`, that project is compiled
/// directly **without** changing the active project in state; otherwise the
/// currently active project is compiled.
/// Collects compiler broadcast output and returns it as a `CompileOutput`.
pub async fn cmd_compile(
    rebuild: bool,
    project_id: Option<usize>,
    filter: CompileFilterOptions,
) -> Result<CompileOutput> {
    cmd_compile_with_progress(rebuild, project_id, filter, None).await
}

/// Compiles a project selected by a reference (project name or numeric id).
///
/// `None` compiles the active project. A reference that uniquely identifies a
/// project compiles it; a reference matching several projects returns
/// [`CompileOrAmbiguity::Ambiguity`] (the candidate list) **without** compiling;
/// a reference matching nothing is an error.
pub async fn cmd_compile_ref(
    rebuild: bool,
    project: Option<String>,
    filter: CompileFilterOptions,
) -> Result<CompileOrAmbiguity> {
    cmd_compile_ref_with_progress(rebuild, project, filter, None).await
}

/// Like [`cmd_compile_ref`] but streams each compiler output line to
/// `on_progress` as it arrives (used by the CLI for live output).
pub async fn cmd_compile_ref_with_progress(
    rebuild: bool,
    project: Option<String>,
    filter: CompileFilterOptions,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOrAmbiguity> {
    let project_id: Option<usize> = match project {
        None => None,
        Some(reference) => {
            let data = PROJECTS_DATA.read().await;
            match resolve_project_reference(&data, &reference) {
                ProjectResolution::Single(id) => Some(id),
                ProjectResolution::Ambiguous(matches) => {
                    return Ok(CompileOrAmbiguity::Ambiguity(AmbiguousProjects {
                        reference,
                        matches,
                    }));
                }
                ProjectResolution::NotFound => {
                    bail!("No project matches \"{reference}\". Use `list` to see available projects.")
                }
            }
        }
    };
    let output = cmd_compile_with_progress(rebuild, project_id, filter, on_progress).await?;
    Ok(CompileOrAmbiguity::Output(output))
}

/// Compiles a project and optionally invokes `on_progress` for each emitted
/// compiler output line as it arrives.
pub async fn cmd_compile_with_progress(
    rebuild: bool,
    project_id: Option<usize>,
    filter: CompileFilterOptions,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOutput> {
    let (project_name, resolved_id, link_id) = {
        let data = PROJECTS_DATA.read().await;
        // Use the explicitly requested project_id, falling back to the active project.
        // We intentionally do NOT call cmd_select_project here so that the active
        // project in state is never changed as a side-effect of a compile call.
        let target_id = match project_id.or(data.active_project_id) {
            Some(id) => id,
            _ => bail!("No active project selected."),
        };
        let project = match data.get_project(target_id) {
            Some(p) => p,
            _ => bail!("Project with ID {target_id} not found."),
        };
        let name = project.name.clone();
        let lid = find_project_link_id(&data, target_id);
        match lid {
            Some(lid) => (name, target_id, lid),
            _ => bail!("Project \"{name}\" has no compiled links."),
        }
    };

    let params = CompileProjectParams::Project {
        project_id: resolved_id,
        project_link_id: Some(link_id),
        rebuild,
        event_id: "cmd-compile".to_string(),
    };

    let compiler = Compiler::new_standalone(&params).await;
    run_compile_collecting(compiler, project_name, filter, on_progress).await
}

/// Compiles a Delphi project from a file path.
///
/// If the file already belongs to a managed project (its `.dproj`/`.dpr`/`.dpk`
/// matches one), it is compiled as that managed project — identical to
/// referencing it by name — and a path shared by several projects yields the
/// candidate list ([`CompileOrAmbiguity::Ambiguity`]) instead of compiling.
/// Only a file owned by no project is compiled **ad-hoc**: an ephemeral
/// [`ProjectsData`] is assembled in memory (a throw-away workspace bound to the
/// chosen compiler) and the regular compile path runs against it, leaving the
/// persisted state untouched.
///
/// `compiler` selects the ad-hoc compiler configuration: matched first as an
/// exact key (e.g. `"12.0"`), then by product name (e.g. `"Delphi 12"`); `None`
/// uses the newest installed compiler. `config` / `platform` are optional
/// ad-hoc build overrides. (These three are ignored for a managed match, which
/// uses the project's own workspace compiler and overrides.)
pub async fn cmd_compile_file(
    file_path: String,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
    rebuild: bool,
    filter: CompileFilterOptions,
) -> Result<CompileOrAmbiguity> {
    cmd_compile_file_with_progress(file_path, compiler, config, platform, rebuild, filter, None)
        .await
}

/// Like [`cmd_compile_file`] but invokes `on_progress` for each emitted
/// compiler output line as it arrives.
pub async fn cmd_compile_file_with_progress(
    file_path: String,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
    rebuild: bool,
    filter: CompileFilterOptions,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOrAmbiguity> {
    // Prefer a managed project that owns this file: compile it like a named
    // reference (with ambiguity reporting) rather than ad-hoc.
    let managed_id = {
        let data = PROJECTS_DATA.read().await;
        match resolve_project_by_path(&data, &file_path) {
            ProjectResolution::Single(id) => Some(id),
            ProjectResolution::Ambiguous(matches) => {
                return Ok(CompileOrAmbiguity::Ambiguity(AmbiguousProjects {
                    reference: file_path,
                    matches,
                }));
            }
            ProjectResolution::NotFound => None,
        }
    };
    if let Some(id) = managed_id {
        let output = cmd_compile_with_progress(rebuild, Some(id), filter, on_progress).await?;
        return Ok(CompileOrAmbiguity::Output(output));
    }

    // Ad-hoc: the file is not part of any managed project.
    if !std::path::Path::new(&file_path).exists() {
        bail!("File not found: {file_path}");
    }
    let compiler_key = resolve_compiler_key(compiler).await?;

    // Assemble the ephemeral, non-persisted project state.
    let mut data = ProjectsData::default();
    data.new_workspace(&"ad-hoc".to_string(), &compiler_key).await?;
    let workspace_id = data.workspaces[0].id;
    data.new_project(&file_path, workspace_id)?;
    let project = data
        .projects
        .last_mut()
        .ok_or_else(|| anyhow::anyhow!("Failed to create ad-hoc project from: {file_path}"))?;
    if config.is_some() {
        project.active_configuration = config;
    }
    if platform.is_some() {
        project.active_platform = platform;
    }
    let project_id = project.id;
    let project_name = project.name.clone();
    let link_id = find_project_link_id(&data, project_id)
        .ok_or_else(|| anyhow::anyhow!("Ad-hoc project link was not created."))?;

    let params = CompileProjectParams::Project {
        project_id,
        project_link_id: Some(link_id),
        rebuild,
        event_id: "cmd-compile-file".to_string(),
    };

    let compiler = Compiler::new_standalone_with_data(&params, data).await;
    let output = run_compile_collecting(compiler, project_name, filter, on_progress).await?;
    Ok(CompileOrAmbiguity::Output(output))
}

/// Drives a prepared [`Compiler`] to completion while collecting (and
/// optionally streaming) its broadcast output, applying the diagnostic
/// filters, and returns the assembled [`CompileOutput`].
async fn run_compile_collecting(
    compiler: Compiler,
    project_name: String,
    filter: CompileFilterOptions,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOutput> {
    // Collect broadcast messages concurrently with compilation.
    let collected: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected_clone = collected.clone();
    let progress_callback = on_progress.clone();
    let filter_opts = filter.clone();
    let mut receiver = CompilerProgress::subscribe();

    let collect_handle = tokio::spawn(async move {
        let mut counts = DiagCounts::default();
        let emit = |callback: &Option<CompileProgressCallback>,
                    lines: &mut Vec<String>,
                    out: Vec<String>| {
            if let Some(cb) = callback {
                for line in &out {
                    cb(line.clone());
                }
            }
            lines.extend(out);
        };
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let mut lines = collected_clone.lock().unwrap();
                    match event {
                        CompilerProgressParams::Start { lines: ls }
                        | CompilerProgressParams::SingleProjectStarted { lines: ls, .. } => {
                            let out = if filter_opts.trim_banners {
                                trim_banner_lines(ls)
                            } else {
                                ls
                            };
                            emit(&progress_callback, &mut lines, out);
                        }
                        CompilerProgressParams::Completed { lines: ls, .. }
                        | CompilerProgressParams::SingleProjectCompleted { lines: ls, .. } => {
                            // Drain pending per-project diagnostic summary first
                            // so it appears immediately before the footer.
                            if filter_opts.summarize_diagnostics {
                                let summary = counts.drain_summary_lines();
                                if !summary.is_empty() {
                                    emit(&progress_callback, &mut lines, summary);
                                }
                            } else {
                                counts.drain_summary_lines();
                            }
                            let out = if filter_opts.trim_banners {
                                trim_banner_lines(ls)
                            } else {
                                ls
                            };
                            emit(&progress_callback, &mut lines, out);
                        }
                        CompilerProgressParams::Stdout { line }
                        | CompilerProgressParams::Stderr { line } => {
                            if let Some((kind, file)) = classify_diagnostic_line(&line) {
                                let suppress = match kind {
                                    DiagKind::Warn => !filter_opts.show_warnings,
                                    DiagKind::Hint => !filter_opts.show_hints,
                                    DiagKind::Error => false,
                                };
                                if suppress {
                                    if filter_opts.summarize_diagnostics {
                                        counts.add(&file, kind);
                                    }
                                    continue;
                                }
                            }
                            emit(&progress_callback, &mut lines, vec![line]);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });

    let compile_result = compiler.compile().await;

    // Brief settling window for in-flight broadcasts, then stop collector.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    collect_handle.abort();
    let _ = collect_handle.await;

    let output_lines = match std::sync::Arc::try_unwrap(collected) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    match compile_result {
        Ok(result) => Ok(CompileOutput {
            project_name,
            success: result.success,
            cancelled: result.cancelled,
            code: result.code,
            lines: output_lines,
        }),
        Err(e) => {
            // Still return collected output on failure.
            if on_progress.is_some() {
                bail!("Compilation failed: {e}");
            }
            bail!(
                "Compilation failed: {e}{}",
                if output_lines.is_empty() {
                    String::new()
                } else {
                    format!("\n\nCompiler output:\n{}", output_lines.join("\n"))
                }
            );
        }
    }
}

/// Formats a Delphi source file in-place.
///
/// Reads the file at `file_path`, decodes it with `encoding` (e.g. `"utf-8"`,
/// `"windows-1252"`, `"oem"`), runs it through the DDK formatter, then
/// encodes the result back to the same encoding before writing.
/// Defaults to `"utf-8"` when `encoding` is `None`.
pub async fn cmd_format_file(file_path: String, encoding: Option<String>) -> Result<FormatFileResult> {
    use crate::format::Formatter;
    use crate::encoding::{decode_bytes, encode_string};

    let encoding_label = encoding.as_deref().unwrap_or("utf-8");

    let raw = std::fs::read(&file_path)
        .with_context(|| format!("Failed to read file: {file_path}"))?;
    let content = decode_bytes(&raw, encoding_label);

    let formatted = Formatter::new(content)?.execute().await?;

    let out_bytes = encode_string(&formatted, encoding_label);
    std::fs::write(&file_path, &out_bytes)
        .with_context(|| format!("Failed to write file: {file_path}"))?;
    Ok(FormatFileResult { file_path })
}
