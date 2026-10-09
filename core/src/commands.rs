//! Shared command implementations for DDK.
//!
//! Both the MCP server and the CLI binary delegate to these functions. Each
//! returns a typed struct; the caller decides how to present it.

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

use crate::files::dproj::{find_dproj_file, get_main_source};
use crate::lsp_types::{CompileProjectParams, CompilerProgress, CompilerProgressParams};
use crate::projects::*;
use crate::state::*;
use crate::utils::normalize_path;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: usize,
    pub name: String,
    pub directory: String,
    pub dproj: Option<String>,
    pub exe: Option<String>,
    /// DevKit override or the dproj's `Debugger_HostApplication`: the exe that
    /// hosts a project without a standalone one (a package or DLL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: usize,
    pub name: String,
    pub compiler_id: String,
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupProjectSummary {
    pub name: String,
    pub path: String,
    pub compiler_id: String,
    pub projects: Vec<ProjectSummary>,
}

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
                    if let Some(host) = &p.host {
                        writeln!(f, "       host: {host}")?;
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
                    if let Some(host) = &p.host {
                        writeln!(f, "       host: {host}")?;
                    }
                }
            }
        }

        Ok(())
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentCompilerEntry {
    /// Workspace name, or `"group_project"`.
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

/// Severity is not a field: it is the [`CompileDiagnostics`] group the entry
/// lives in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileDiagnostic {
    pub code: String,
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    pub message: String,
}

/// Errors always appear; warnings and hints only under `show_warnings` /
/// `show_hints`, which gate the structured data and the streamed lines alike.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileDiagnostics {
    pub errors: Vec<CompileDiagnostic>,
    pub warnings: Vec<CompileDiagnostic>,
    pub hints: Vec<CompileDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileOutput {
    pub project: String,
    pub project_path: String,
    pub compiler: String,
    pub config: Option<String>,
    pub platform: Option<String>,
    /// `"compile"` (Clean;Make) or `"rebuild"` (Clean;Build).
    pub action: String,
    pub success: bool,
    pub code: i32,
    #[serde(default)]
    pub diagnostics: CompileDiagnostics,
}

pub type CompileProgressCallback = std::sync::Arc<dyn Fn(String) + Send + Sync>;

/// Only CLI and MCP filter here; the VS Code extension reads compiler events
/// from the LSP server and bypasses these commands entirely.
#[derive(Debug, Clone, Default)]
pub struct CompileFilterOptions {
    /// Drop the banners' box-drawing borders and the centering padding.
    pub trim_banners: bool,
    pub show_warnings: bool,
    pub show_hints: bool,
    /// Emit `<file>: X warn, Y hint` per project for the diagnostics that were
    /// suppressed above.
    pub summarize_diagnostics: bool,
}

impl fmt::Display for CompileOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            write!(f, "Project \"{}\" compiled successfully.", self.project)?;
        } else {
            write!(
                f,
                "Compilation of \"{}\" finished with errors (exit code {}).",
                self.project, self.code
            )?;
        }
        let d = &self.diagnostics;
        if !d.errors.is_empty() || !d.warnings.is_empty() || !d.hints.is_empty() {
            write!(
                f,
                " ({} errors, {} warnings, {} hints)",
                d.errors.len(),
                d.warnings.len(),
                d.hints.len()
            )?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output-filter helpers (used by cmd_compile_with_progress)
// ---------------------------------------------------------------------------

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

fn trim_banner_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|l| !is_banner_border_line(l))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

lazy_static::lazy_static! {
    /// Must track `CompilerLineDiagnostic::Display`, which emits
    /// `HH:MM:SS.mmm: [KIND][CODE] file:line[:col] - message`.
    static ref FORMATTED_DIAG_FULL_REGEX: regex::Regex = regex::Regex::new(
        r"^\d{2}:\d{2}:\d{2}\.\d+:\s+\[(?P<kind>WARN|HINT|ERROR)\]\[(?P<code>[A-Z]\d+)\]\s+(?P<file>.+?):(?P<line>\d+)(?::\d+)?\s+-\s(?P<message>.*)$"
    ).unwrap();
}

fn parse_formatted_diagnostic(line: &str) -> Option<(DiagKind, CompileDiagnostic)> {
    let caps = FORMATTED_DIAG_FULL_REGEX.captures(line)?;
    let kind = match caps.name("kind")?.as_str() {
        "WARN" => DiagKind::Warn,
        "HINT" => DiagKind::Hint,
        "ERROR" => DiagKind::Error,
        _ => return None,
    };
    let diag = CompileDiagnostic {
        code: caps.name("code")?.as_str().to_string(),
        file: caps.name("file")?.as_str().to_string(),
        line: caps.name("line")?.as_str().parse().ok()?,
        message: caps.name("message")?.as_str().to_string(),
    };
    Some((kind, diag))
}

#[cfg(test)]
mod diagnostics_parse_tests {
    use super::*;

    #[test]
    fn parses_warning_line() {
        let line = "15:55:33.909: [WARN][W1035] C:\\proj\\Hello.dpr:7 - \
                    Rückgabewert der Funktion 'F' könnte undefiniert sein";
        let (kind, d) = parse_formatted_diagnostic(line).unwrap();
        assert_eq!(kind, DiagKind::Warn);
        assert_eq!(d.code, "W1035");
        assert_eq!(d.file, "C:\\proj\\Hello.dpr");
        assert_eq!(d.line, 7);
        assert!(d.message.contains("Rückgabewert"));
    }

    #[test]
    fn parses_error_line_with_column() {
        let line = "10:00:00.000: [ERROR][E2003] C:\\a\\B.pas:12:5 - Undeclared identifier";
        let (kind, d) = parse_formatted_diagnostic(line).unwrap();
        assert_eq!(kind, DiagKind::Error);
        assert_eq!(d.code, "E2003");
        assert_eq!(d.line, 12);
        assert_eq!(d.message, "Undeclared identifier");
    }

    #[test]
    fn parses_hint_line() {
        let line = "10:00:00.000: [HINT][H2077] C:\\a\\B.pas:3 - Value assigned to 'x' never used";
        let (kind, _d) = parse_formatted_diagnostic(line).unwrap();
        assert_eq!(kind, DiagKind::Hint);
    }

    #[test]
    fn ignores_non_diagnostic_line() {
        assert!(parse_formatted_diagnostic("just some banner text").is_none());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagKind {
    Warn,
    Hint,
    Error,
}

/// Filename without extension; accepts both `/` and `\` separators.
fn diag_file_basename(path: &str) -> String {
    let last = path.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(path);
    match last.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => last.to_string(),
    }
}

/// Warnings/hints suppressed from the streamed output, counted per file
/// basename in arrival order.
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

/// Case-insensitive path equality on normalized forms.
fn paths_equal_ci(a: &str, b: &str) -> bool {
    normalize_path(a).to_string_lossy().to_lowercase() == normalize_path(b).to_string_lossy().to_lowercase()
}

/// First matching `ProjectLink.id`: workspaces are searched before the group
/// project.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: usize,
    pub name: String,
    /// Workspace or group-project name, or `"(unlinked)"`.
    pub location: String,
    /// The project's `.dproj`/`.dpr`/`.dpk`, falling back to its directory.
    pub path: String,
}

#[derive(Debug, Clone)]
pub enum ProjectResolution {
    Single(usize),
    Ambiguous(Vec<ProjectRef>),
    NotFound,
}

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

/// An existing numeric id wins outright; otherwise an exact name beats a
/// substring match, both case-insensitive. Several matches are returned as
/// candidates rather than picked.
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

/// Resolve a file path to the managed project(s) whose `.dproj`/`.dpr`/`.dpk`
/// it is. Paths are normalised and compared case-insensitively.
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

/// Matching order: exact key (`"12.0"`) → exact product name → unique
/// product-name substring, the latter two case-insensitive. `None` takes
/// `"12.0"` when installed, else the highest `compiler_version`.
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

/// Matching order: exact name → case-insensitive name → numeric id.
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

pub async fn cmd_list_projects() -> Result<ProjectListResult> {
    let projects_data = PROJECTS_DATA.read().await;
    let active_id = projects_data.active_project_id;

    let make_summary = |p: &crate::projects::Project| ProjectSummary {
        id: p.id,
        name: p.name.clone(),
        directory: p.directory.clone(),
        dproj: p.dproj.clone(),
        exe: p.exe.clone(),
        // Only surface a host that differs from the project's own exe.
        host: p.effective_host_application().filter(|host| {
            !p.exe.as_deref().is_some_and(|exe| paths_equal_ci(host, exe))
        }),
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

/// `file_path` may be a `.dproj`, `.dpr` or `.dpk`; a bare source without a
/// `.dproj` works too.
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

/// `compiler` resolves like everywhere else: exact key, exact product name, or
/// a unique product-name substring.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatFileResult {
    pub file_path: String,
}

impl fmt::Display for FormatFileResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Formatted: {}", self.file_path)
    }
}

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
        write!(f, "Re-run targeting the specific project ID.")
    }
}

#[derive(Debug, Clone)]
pub enum CompileOrAmbiguity {
    Output(CompileOutput),
    Ambiguity(AmbiguousProjects),
}

/// `project_id` compiles that project without making it the active one;
/// `None` compiles the active project.
///
/// `debug_info` forces the full debug artefact set (optimizations off, TD32
/// debug info, `.rsm`, detailed `.map`) over the build configuration's own
/// settings, without touching the dproj.
pub async fn cmd_compile(
    rebuild: bool,
    debug_info: bool,
    project_id: Option<usize>,
    filter: CompileFilterOptions,
) -> Result<CompileOutput> {
    cmd_compile_with_progress(rebuild, debug_info, project_id, filter, Vec::new(), None).await
}

/// `project` is a name or numeric id; `None` compiles the active project. An
/// ambiguous reference returns the candidates without compiling, an unmatched
/// one is an error.
pub async fn cmd_compile_ref(
    rebuild: bool,
    debug_info: bool,
    project: Option<String>,
    filter: CompileFilterOptions,
    extra_msbuild_args: Vec<String>,
) -> Result<CompileOrAmbiguity> {
    cmd_compile_ref_with_progress(rebuild, debug_info, project, filter, extra_msbuild_args, None).await
}

/// Like [`cmd_compile_ref`] but streams each compiler output line to
/// `on_progress` as it arrives (used by the CLI for live output).
pub async fn cmd_compile_ref_with_progress(
    rebuild: bool,
    debug_info: bool,
    project: Option<String>,
    filter: CompileFilterOptions,
    extra_msbuild_args: Vec<String>,
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
    let output =
        cmd_compile_with_progress(rebuild, debug_info, project_id, filter, extra_msbuild_args, on_progress)
            .await?;
    Ok(CompileOrAmbiguity::Output(output))
}

pub async fn cmd_compile_with_progress(
    rebuild: bool,
    debug_info: bool,
    project_id: Option<usize>,
    filter: CompileFilterOptions,
    extra_msbuild_args: Vec<String>,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOutput> {
    let (project_name, resolved_id, link_id) = {
        let data = PROJECTS_DATA.read().await;
        // Deliberately not cmd_select_project: compiling must not change the
        // active project.
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
        debug_info,
        event_id: "cmd-compile".to_string(),
    };

    let compiler = Compiler::new_standalone(&params)
        .await
        .with_extra_msbuild_args(extra_msbuild_args);
    run_compile_collecting(compiler, project_name, filter, on_progress).await
}

/// A file owned by a managed project compiles as that project, exactly as a
/// reference by name would; only a file owned by none is compiled ad-hoc
/// against ephemeral state, leaving the persisted state untouched.
///
/// `compiler`, `config` and `platform` configure that ad-hoc build and are
/// ignored for a managed match, which uses the project's own workspace
/// compiler and overrides.
pub async fn cmd_compile_file(
    file_path: String,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
    rebuild: bool,
    debug_info: bool,
    filter: CompileFilterOptions,
    extra_msbuild_args: Vec<String>,
) -> Result<CompileOrAmbiguity> {
    cmd_compile_file_with_progress(
        file_path,
        compiler,
        config,
        platform,
        rebuild,
        debug_info,
        filter,
        extra_msbuild_args,
        None,
    )
    .await
}

pub async fn cmd_compile_file_with_progress(
    file_path: String,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
    rebuild: bool,
    debug_info: bool,
    filter: CompileFilterOptions,
    extra_msbuild_args: Vec<String>,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOrAmbiguity> {
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
        let output =
            cmd_compile_with_progress(rebuild, debug_info, Some(id), filter, extra_msbuild_args, on_progress)
                .await?;
        return Ok(CompileOrAmbiguity::Output(output));
    }

    let data = adhoc_project_data(&file_path, compiler, config, platform).await?;
    let project = data.projects.last().expect("adhoc_project_data holds the project");
    let project_id = project.id;
    let project_name = project.name.clone();
    let link_id = find_project_link_id(&data, project_id)
        .ok_or_else(|| anyhow::anyhow!("Ad-hoc project link was not created."))?;

    let params = CompileProjectParams::Project {
        project_id,
        project_link_id: Some(link_id),
        rebuild,
        debug_info,
        event_id: "cmd-compile-file".to_string(),
    };

    let compiler = Compiler::new_standalone_with_data(&params, data)
        .await
        .with_extra_msbuild_args(extra_msbuild_args);
    let output = run_compile_collecting(compiler, project_name, filter, on_progress).await?;
    Ok(CompileOrAmbiguity::Output(output))
}

/// Drives a prepared [`Compiler`] to completion, collecting and optionally
/// streaming its broadcast output under the diagnostic filters.
async fn run_compile_collecting(
    compiler: Compiler,
    project_name: String,
    filter: CompileFilterOptions,
    on_progress: Option<CompileProgressCallback>,
) -> Result<CompileOutput> {
    let diagnostics: std::sync::Arc<std::sync::Mutex<CompileDiagnostics>> =
        std::sync::Arc::new(std::sync::Mutex::new(CompileDiagnostics::default()));
    let diagnostics_clone = diagnostics.clone();
    let progress_callback = on_progress.clone();
    let filter_opts = filter.clone();
    let mut receiver = CompilerProgress::subscribe();

    let collect_handle = tokio::spawn(async move {
        let mut counts = DiagCounts::default();
        let stream = |callback: &Option<CompileProgressCallback>, out: Vec<String>| {
            if let Some(cb) = callback {
                for line in &out {
                    cb(line.clone());
                }
            }
        };
        loop {
            match receiver.recv().await {
                Ok(event) => match event {
                    CompilerProgressParams::Start { lines: ls }
                    | CompilerProgressParams::SingleProjectStarted { lines: ls, .. } => {
                        let out = if filter_opts.trim_banners {
                            trim_banner_lines(ls)
                        } else {
                            ls
                        };
                        stream(&progress_callback, out);
                    }
                    CompilerProgressParams::Completed { lines: ls, .. }
                    | CompilerProgressParams::SingleProjectCompleted { lines: ls, .. } => {
                        // Drain first, so the summary precedes the footer.
                        if filter_opts.summarize_diagnostics {
                            let summary = counts.drain_summary_lines();
                            if !summary.is_empty() {
                                stream(&progress_callback, summary);
                            }
                        } else {
                            counts.drain_summary_lines();
                        }
                        let out = if filter_opts.trim_banners {
                            trim_banner_lines(ls)
                        } else {
                            ls
                        };
                        stream(&progress_callback, out);
                    }
                    CompilerProgressParams::Stdout { line }
                    | CompilerProgressParams::Stderr { line } => {
                        if let Some((kind, diag)) = parse_formatted_diagnostic(&line) {
                            // A suppressed severity appears neither in the
                            // structured diagnostics nor in the streamed lines.
                            let suppress = match kind {
                                DiagKind::Warn => !filter_opts.show_warnings,
                                DiagKind::Hint => !filter_opts.show_hints,
                                DiagKind::Error => false,
                            };
                            if suppress {
                                if filter_opts.summarize_diagnostics {
                                    counts.add(&diag_file_basename(&diag.file), kind);
                                }
                                continue;
                            }
                            let mut d = diagnostics_clone.lock().unwrap();
                            match kind {
                                DiagKind::Error => d.errors.push(diag),
                                DiagKind::Warn => d.warnings.push(diag),
                                DiagKind::Hint => d.hints.push(diag),
                            }
                        }
                        stream(&progress_callback, vec![line]);
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                // Known gap: the broadcast has no backpressure, so a producer
                // outrunning this loop overwrites events and their diagnostics
                // are lost from the JSON without a trace. Closing it means
                // giving the compiler output a channel that blocks instead of
                // dropping — a change to the shared CompilerProgress broadcast.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });

    let compile_result = compiler.compile().await;

    // Known gap: everything is sent by now, but the collector may still be
    // working through its backlog, and the abort cuts whatever it has not
    // processed after 100 ms. Draining until the channel is empty is not the
    // fix on its own: the broadcast's sender is a process-wide OnceLock and
    // never closes, so a termination signal has to come first.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    collect_handle.abort();
    let _ = collect_handle.await;

    let output_diagnostics = match std::sync::Arc::try_unwrap(diagnostics) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    match compile_result {
        Ok(result) => Ok(CompileOutput {
            project: project_name,
            project_path: result.header.target,
            compiler: result.header.compiler,
            config: result.header.config,
            platform: result.header.platform,
            action: if result.header.rebuild { "rebuild" } else { "compile" }.to_string(),
            success: result.success,
            code: result.code,
            diagnostics: output_diagnostics,
        }),
        Err(e) => bail!("Compilation failed: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutput {
    /// `None` when a bare `.exe` path was run without a project.
    pub project_name: Option<String>,
    pub exe: String,
    pub args: Vec<String>,
}

impl fmt::Display for RunOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.project_name {
            Some(name) => write!(f, "Running \"{name}\": {}", self.exe)?,
            _ => write!(f, "Running: {}", self.exe)?,
        }
        if !self.args.is_empty() {
            write!(f, " {}", self.args.join(" "))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum RunOrAmbiguity {
    Output(RunOutput),
    Ambiguity(AmbiguousProjects),
}

/// The dproj's `Debugger_RunParams` and the DDK "Start Parameters" override
/// both contribute, base first: neither discards the other.
pub(crate) fn fuse_run_params(base: Option<String>, extra: Option<String>) -> Option<String> {
    let base = base.filter(|s| !s.trim().is_empty());
    let extra = extra.filter(|s| !s.trim().is_empty());
    match (base, extra) {
        (Some(base), Some(extra)) => Some(format!("{base} {extra}")),
        (Some(base), None) => Some(base),
        (None, Some(extra)) => Some(extra),
        (None, None) => None,
    }
}

/// Splits into argv entries, honoring double-quoted segments.
pub(crate) fn split_run_args(args: &str) -> Vec<String> {
    let re = Regex::new(r#""([^"]*)"|(\S+)"#).unwrap();
    re.captures_iter(args)
        .map(|c| c.get(1).or_else(|| c.get(2)).unwrap().as_str().to_string())
        .collect()
}

/// Detached: the child is not awaited and outlives this call.
fn launch_executable(exe_path: &str, args: &[String]) -> Result<()> {
    let exe = std::path::Path::new(exe_path);
    if !exe.exists() {
        bail!("Executable not found: {exe_path}");
    }
    let mut command = std::process::Command::new(exe);
    command.args(args);
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    command
        .spawn()
        .with_context(|| format!("Failed to launch executable: {exe_path}"))?;
    Ok(())
}

/// `args` replaces the project's fused run parameters for this invocation
/// only.
pub async fn cmd_run(project_id: Option<usize>, args: Option<String>) -> Result<RunOutput> {
    let (project_name, exe, start_parameters) = {
        let data = PROJECTS_DATA.read().await;
        let target_id = match project_id.or(data.active_project_id) {
            Some(id) => id,
            _ => bail!("No active project selected."),
        };
        let project = match data.get_project(target_id) {
            Some(p) => p,
            _ => bail!("Project with ID {target_id} not found."),
        };
        // A configured Host Application wins over the project's own exe, as in
        // the IDE: it is what makes a `.dpk` package or DLL project runnable.
        let exe = match project.effective_host_application().or_else(|| project.exe.clone()) {
            Some(target) => target,
            _ => bail!(
                "Project \"{}\" has no executable or Host Application. Compile it first, set its .exe path, or set a Host Application.",
                project.name
            ),
        };
        let start_parameters = fuse_run_params(project.dproj_run_params.clone(), project.start_parameters.clone());
        (project.name.clone(), exe, start_parameters)
    };
    let raw_args = args.or(start_parameters).unwrap_or_default();
    let parsed_args = split_run_args(&raw_args);
    launch_executable(&exe, &parsed_args)?;
    Ok(RunOutput { project_name: Some(project_name), exe, args: parsed_args })
}

/// `project` is a name or numeric id; `None` runs the active project. An
/// ambiguous reference returns the candidates without running, an unmatched
/// one is an error.
pub async fn cmd_run_ref(project: Option<String>, args: Option<String>) -> Result<RunOrAmbiguity> {
    let project_id: Option<usize> = match project {
        None => None,
        Some(reference) => {
            let data = PROJECTS_DATA.read().await;
            match resolve_project_reference(&data, &reference) {
                ProjectResolution::Single(id) => Some(id),
                ProjectResolution::Ambiguous(matches) => {
                    return Ok(RunOrAmbiguity::Ambiguity(AmbiguousProjects { reference, matches }));
                }
                ProjectResolution::NotFound => {
                    bail!("No project matches \"{reference}\". Use `list` to see available projects.")
                }
            }
        }
    };
    Ok(RunOrAmbiguity::Output(cmd_run(project_id, args).await?))
}

/// Unlike `compile`, a file owned by no managed project is an error: there is
/// nothing to execute until the project has been compiled.
pub async fn cmd_run_file(file_path: String, args: Option<String>) -> Result<RunOrAmbiguity> {
    let managed_id = {
        let data = PROJECTS_DATA.read().await;
        match resolve_project_by_path(&data, &file_path) {
            ProjectResolution::Single(id) => Some(id),
            ProjectResolution::Ambiguous(matches) => {
                return Ok(RunOrAmbiguity::Ambiguity(AmbiguousProjects { reference: file_path, matches }));
            }
            ProjectResolution::NotFound => None,
        }
    };
    match managed_id {
        Some(id) => Ok(RunOrAmbiguity::Output(cmd_run(Some(id), args).await?)),
        _ => bail!(
            "\"{file_path}\" is not part of a managed project. Add it to a workspace first, or run its .exe path directly."
        ),
    }
}

/// Runs an executable directly, bypassing project resolution.
pub async fn cmd_run_exe(exe_path: String, args: Option<String>) -> Result<RunOutput> {
    let parsed_args = args.map(|a| split_run_args(&a)).unwrap_or_default();
    launch_executable(&exe_path, &parsed_args)?;
    Ok(RunOutput { project_name: None, exe: exe_path, args: parsed_args })
}

/// The one extension-dispatch rule CLI and MCP share: `.exe` runs directly,
/// `.dproj`/`.dpr`/`.dpk` resolves to its managed project.
pub async fn cmd_run_path(path: String, args: Option<String>) -> Result<RunOrAmbiguity> {
    if has_extension(&path, &["exe"]) {
        return Ok(RunOrAmbiguity::Output(cmd_run_exe(path, args).await?));
    }
    if is_delphi_project_path(&path) {
        return cmd_run_file(path, args).await;
    }
    bail!("\"{path}\" is not a recognized project or executable file (expected .dproj/.dpr/.dpk/.exe).");
}

// ---------------------------------------------------------------------------
// Debug target
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum DebugTargetOrAmbiguity {
    Target(crate::debug_target::DebugTarget),
    Ambiguity(AmbiguousProjects),
}

/// Describes a project's debug target — see [`crate::debug_target`] — by
/// numeric id, name, or path; `None` targets the active project. `config` and
/// `platform` are the same overrides `compile` takes, so the artefacts
/// described are the ones such a build would produce. Nothing is persisted.
pub async fn cmd_debug_target(
    reference: Option<String>,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
) -> Result<DebugTargetOrAmbiguity> {
    let data = PROJECTS_DATA.read().await;
    let project_id = match &reference {
        None => match data.active_project_id {
            Some(id) => id,
            _ => bail!("No active project selected."),
        },
        Some(reference) if is_delphi_project_path(reference) => match resolve_project_by_path(&data, reference) {
            ProjectResolution::Single(id) => id,
            ProjectResolution::Ambiguous(matches) => {
                return Ok(DebugTargetOrAmbiguity::Ambiguity(AmbiguousProjects {
                    reference: reference.clone(),
                    matches,
                }));
            }
            ProjectResolution::NotFound => {
                drop(data);
                let target = adhoc_debug_target(reference, compiler, config, platform).await?;
                return Ok(DebugTargetOrAmbiguity::Target(target));
            }
        },
        Some(reference) => match resolve_project_reference(&data, reference) {
            ProjectResolution::Single(id) => id,
            ProjectResolution::Ambiguous(matches) => {
                return Ok(DebugTargetOrAmbiguity::Ambiguity(AmbiguousProjects {
                    reference: reference.clone(),
                    matches,
                }));
            }
            ProjectResolution::NotFound => {
                bail!("No project matches \"{reference}\". Use `list` to see available projects.")
            }
        },
    };
    let project = match data.get_project(project_id) {
        Some(p) => p,
        _ => bail!("Project with ID {project_id} not found."),
    };
    // An orphan project has no compiler of its own; describing it is read-only,
    // so fall back to the requested or newest one and note it rather than
    // refusing. A linked project ignores `compiler`, which is noted as well.
    let (compiler, compiler_note) = match data.compiler_for_project(project_id).await {
        Some(own) => {
            let note = compiler.map(|requested| {
                format!(
                    "`compiler` ({requested}) was ignored: project \"{}\" builds with {}, the compiler of the \
                     workspace or group project it belongs to.",
                    project.name, own.product_name
                )
            });
            (own, note)
        }
        _ => {
            let key = resolve_compiler_key(compiler).await?;
            let compilers = COMPILER_CONFIGURATIONS.read().await;
            let fallback = compilers
                .get(&key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Compiler configuration \"{key}\" not found."))?;
            let note = format!(
                "Project \"{}\" is linked to no workspace or group project; described with {} ({key}).",
                project.name, fallback.product_name
            );
            (fallback, Some(note))
        }
    };
    // What follows is synchronous filesystem work, and one search path on a
    // disconnected share blocks it for the SMB timeout. Copy the project and
    // release the lock first, so a slow describe costs only its own caller.
    let project = project.clone();
    drop(data);
    let target = tokio::task::spawn_blocking(move || {
        let (described, discovery_warnings) =
            crate::debug_target::project_to_describe(&project, config, platform, &compiler.ide_environment_overrides());
        crate::debug_target::build_debug_target(&described, &compiler).map(|target| (target, discovery_warnings))
    })
    .await;
    let (mut target, discovery_warnings) = match target {
        Ok(described) => described?,
        Err(error) => bail!("Describing the debug target did not finish: {error}"),
    };
    target.warnings.splice(0..0, discovery_warnings);
    target.notes.splice(0..0, compiler_note);
    Ok(DebugTargetOrAmbiguity::Target(target))
}

/// Describes the same ephemeral project [`cmd_compile_file`] would build.
async fn adhoc_debug_target(
    file_path: &str,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
) -> Result<crate::debug_target::DebugTarget> {
    let data = adhoc_project_data(file_path, compiler, config, platform).await?;
    let project = data.projects.last().expect("adhoc_project_data holds the project");
    let compiler = data
        .compiler_for_project(project.id)
        .await
        .ok_or_else(|| anyhow::anyhow!("Ad-hoc project link was not created."))?;
    let mut target = crate::debug_target::build_debug_target(project, &compiler)?;
    target.project_id = None;
    Ok(target)
}

/// The ephemeral, never-persisted state behind every ad-hoc command: one
/// workspace on `compiler` holding the file as its only project, which
/// callers take from the end of `projects`.
async fn adhoc_project_data(
    file_path: &str,
    compiler: Option<String>,
    config: Option<String>,
    platform: Option<String>,
) -> Result<ProjectsData> {
    if !std::path::Path::new(file_path).exists() {
        bail!("File not found: {file_path}");
    }
    let compiler_key = resolve_compiler_key(compiler).await?;
    let mut data = ProjectsData::default();
    data.new_workspace(&"ad-hoc".to_string(), &compiler_key).await?;
    let workspace_id = data.workspaces[0].id;
    let ide_env = data.ide_environment_for_workspace(workspace_id).await;
    data.new_project(&file_path.to_string(), workspace_id, &ide_env)?;
    let project = data
        .projects
        .last_mut()
        .ok_or_else(|| anyhow::anyhow!("Failed to create ad-hoc project from: {file_path}"))?;
    if config.is_none() && platform.is_none() {
        return Ok(data);
    }
    if config.is_some() {
        project.active_configuration = config;
    }
    if platform.is_some() {
        project.active_platform = platform;
    }
    // `new_project` discovered the paths of the dproj's default build, not of
    // the configuration and platform just requested.
    project.discover_paths(&ide_env)?;
    Ok(data)
}

fn is_delphi_project_path(value: &str) -> bool {
    has_extension(value, &["dproj", "dpr", "dpk"])
}

fn has_extension(value: &str, extensions: &[&str]) -> bool {
    std::path::Path::new(value)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.iter().any(|known| ext.eq_ignore_ascii_case(known)))
}

// ---------------------------------------------------------------------------
// DelphiLSP settings file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum DelphiLspOrAmbiguity {
    Output(crate::delphilsp::DelphiLspConfigResult),
    Ambiguity(AmbiguousProjects),
}

async fn delphilsp_request_for_project(
    project_id: usize,
    out: Option<String>,
) -> Result<crate::delphilsp::GenerationRequest> {
    let data = PROJECTS_DATA.read().await;
    let project = match data.get_project(project_id) {
        Some(p) => p,
        _ => bail!("Project with ID {project_id} not found."),
    };
    let compiler = match data.compiler_for_project(project_id).await {
        Some(c) => c,
        _ => bail!(
            "Project \"{}\" is not linked to a workspace or the group project, so no compiler can be determined.",
            project.name
        ),
    };
    let dproj_path = project.dproj.as_ref().map(PathBuf::from);
    let main_source = project
        .dpr
        .as_ref()
        .or(project.dpk.as_ref())
        .map(PathBuf::from)
        .or_else(|| dproj_path.as_ref().and_then(|p| crate::files::dproj::get_main_source(p).ok()));
    let main_source = match main_source {
        Some(path) => path,
        _ => bail!(
            "Project \"{}\" has no .dpr/.dpk main source to describe.",
            project.name
        ),
    };
    Ok(crate::delphilsp::GenerationRequest {
        dproj_path,
        main_source,
        configuration: project.active_configuration.clone(),
        platform: project.active_platform.clone(),
        installation_path: PathBuf::from(&compiler.installation_path),
        bds_version: format!("{}.0", compiler.product_version),
        compiler_name: compiler.product_name.clone(),
        out_path: out.map(PathBuf::from),
    })
}

/// Ad-hoc counterpart of [`delphilsp_request_for_project`]: the persisted
/// project state is neither read nor added to.
async fn delphilsp_request_for_path(
    file_path: &str,
    compiler: Option<String>,
    out: Option<String>,
) -> Result<crate::delphilsp::GenerationRequest> {
    let path = normalize_path(file_path);
    if !path.exists() {
        bail!("File not found: {file_path}");
    }
    let is_dproj = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("dproj"))
        .unwrap_or(false);
    let (dproj_path, main_source) = if is_dproj {
        (Some(path.clone()), get_main_source(&path)?)
    } else {
        (find_dproj_file(&path).ok(), path.clone())
    };

    let compiler_key = resolve_compiler_key(compiler).await?;
    let configs = COMPILER_CONFIGURATIONS.read().await;
    let config = match configs.get(&compiler_key) {
        Some(c) => c,
        _ => bail!("Compiler configuration \"{compiler_key}\" disappeared."),
    };
    Ok(crate::delphilsp::GenerationRequest {
        dproj_path,
        main_source,
        configuration: None,
        platform: None,
        installation_path: PathBuf::from(&config.installation_path),
        bds_version: format!("{}.0", config.product_version),
        compiler_name: config.product_name.clone(),
        out_path: out.map(PathBuf::from),
    })
}

/// Generates the `.delphilsp.json` that Embarcadero's DelphiLSP VS Code
/// extension reads for search paths, defines and unit scope names, so code
/// insight works without ever opening the RAD Studio IDE.
///
/// `target` resolves like the compile commands. The file is written next to
/// the project's main source as `<stem>.delphilsp.json` unless `out` says
/// otherwise.
pub async fn cmd_delphilsp_config(
    target: Option<String>,
    compiler: Option<String>,
    out: Option<String>,
) -> Result<DelphiLspOrAmbiguity> {
    let request = match target {
        None => {
            let active_id = {
                let data = PROJECTS_DATA.read().await;
                data.active_project_id
            };
            match active_id {
                Some(id) => delphilsp_request_for_project(id, out).await?,
                _ => bail!("No active project selected."),
            }
        }
        Some(reference) if is_delphi_project_path(&reference) => {
            let managed_id = {
                let data = PROJECTS_DATA.read().await;
                match resolve_project_by_path(&data, &reference) {
                    ProjectResolution::Single(id) => Some(id),
                    ProjectResolution::Ambiguous(matches) => {
                        return Ok(DelphiLspOrAmbiguity::Ambiguity(AmbiguousProjects {
                            reference,
                            matches,
                        }));
                    }
                    ProjectResolution::NotFound => None,
                }
            };
            match managed_id {
                Some(id) => delphilsp_request_for_project(id, out).await?,
                _ => delphilsp_request_for_path(&reference, compiler, out).await?,
            }
        }
        Some(reference) => {
            let resolved = {
                let data = PROJECTS_DATA.read().await;
                resolve_project_reference(&data, &reference)
            };
            match resolved {
                ProjectResolution::Single(id) => delphilsp_request_for_project(id, out).await?,
                ProjectResolution::Ambiguous(matches) => {
                    return Ok(DelphiLspOrAmbiguity::Ambiguity(AmbiguousProjects {
                        reference,
                        matches,
                    }));
                }
                ProjectResolution::NotFound => bail!(
                    "No project matches \"{reference}\". Use `list` to see available projects."
                ),
            }
        }
    };

    Ok(DelphiLspOrAmbiguity::Output(crate::delphilsp::generate(&request)?))
}

/// Formats a Delphi source file in-place. The file is written back in the
/// `encoding` it was decoded with (e.g. `"windows-1252"`, `"oem"`); `None`
/// means `"utf-8"`.
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

#[cfg(test)]
mod fuse_run_params_tests {
    use super::fuse_run_params;

    #[test]
    fn both_present_joins_base_then_extra() {
        assert_eq!(
            fuse_run_params(Some("/STANDALONE".to_string()), Some("-extra flag".to_string())),
            Some("/STANDALONE -extra flag".to_string())
        );
    }

    #[test]
    fn only_base_present() {
        assert_eq!(fuse_run_params(Some("/STANDALONE".to_string()), None), Some("/STANDALONE".to_string()));
    }

    #[test]
    fn only_extra_present() {
        assert_eq!(fuse_run_params(None, Some("-extra".to_string())), Some("-extra".to_string()));
    }

    #[test]
    fn neither_present_is_none() {
        assert_eq!(fuse_run_params(None, None), None);
    }

    #[test]
    fn blank_values_are_treated_as_absent() {
        assert_eq!(
            fuse_run_params(Some("   ".to_string()), Some("-extra".to_string())),
            Some("-extra".to_string())
        );
        assert_eq!(fuse_run_params(Some("".to_string()), Some("".to_string())), None);
    }
}
