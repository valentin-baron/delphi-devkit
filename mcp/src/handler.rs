use async_trait::async_trait;
use rust_mcp_sdk::{
    McpServer,
    macros,
    mcp_server::ServerHandler,
    schema::{
        schema_utils::CallToolError, CallToolRequestParams, CallToolResult,
        ListToolsResult, PaginatedRequestParams, RpcError, TextContent,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use ddk_core::commands;
use ddk_core::commands::CompileFilterOptions;

// ---------------------------------------------------------------------------
// README content embedded at compile time
// ---------------------------------------------------------------------------

static README_CONTENT: &str = include_str!("../../README.md");

// ---------------------------------------------------------------------------
// Tool input types (mcp_tool! generates ::tool() returning a Tool definition)
// ---------------------------------------------------------------------------

#[macros::mcp_tool(
    name = "get_ddk_extension_info",
    description = "Returns the DDK (Delphi Development Kit) extension README, describing all available features, commands, settings, and project views. Use this to understand what the extension can do."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct GetDdkExtensionInfoArgs {}

#[macros::mcp_tool(
    name = "delphi_get_environment_info",
    description = "Returns the currently active Delphi project and its associated compiler configuration. If no project is active, returns only the group project compiler configuration (if any). This information is best presented in a small formatted table. This is only relevant if we are working with Delphi."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct GetEnvironmentInfoArgs {}

#[macros::mcp_tool(
    name = "delphi_list_projects",
    description = "Lists all known Delphi projects grouped by their workspace or group project. Each workspace has its own compiler configuration. Projects are shown with their IDs, names, and paths. Use this to discover available projects and their hierarchy before selecting one."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct ListProjectsArgs {}

#[macros::mcp_tool(
    name = "delphi_select_project",
    description = "Selects a Delphi project by its ID, making it the active project for subsequent operations (compile, run, etc.). Use delphi_list_projects first to discover available project IDs."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct SelectProjectArgs {
    /// The numeric ID of the project to select.
    pub project_id: u64,
}

#[macros::mcp_tool(
    name = "delphi_get_available_compilers",
    description = "Returns all available Delphi compiler configurations with their keys, product names, versions, and installation paths. Use this to discover valid compiler keys before calling delphi_set_group_projects_compiler. If this information is asked for from the user, it is most useful to present it in a clearly formatted table."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct GetAvailableCompilersArgs {}

#[macros::mcp_tool(
    name = "delphi_set_group_projects_compiler",
    description = "Sets the compiler configuration used by the group project. The compiler parameter must be a valid compiler configuration key from the available configurations. Call delphi_get_available_compilers first to discover the available compiler keys."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct SetGroupProjectsCompilerArgs {
    /// The compiler configuration key to set for the group project.
    pub compiler: String,
}

#[macros::mcp_tool(
    name = "delphi_compile_project",
    description = "Compiles a Delphi project (does not change the active project). \
        Target it with `project`: either a numeric ID or a project name (e.g. \"be\"). \
        If a name matches several projects, the tool returns the list of candidates \
        (with their IDs, workspaces and paths) instead of compiling — re-call with the \
        chosen ID. `project_id` is also accepted for an exact numeric target. \
        Omit both to compile the currently active project. \
        Use delphi_list_projects to discover names/IDs. Always match by project name from the user's request. \
        Returns compiler output with the decorative banner stripped. \
        By default warnings and hints are suppressed to save tokens — \
        set show_warnings / show_hints to surface them verbatim, \
        or summarize_diagnostics to receive a per-file `<file>: X warn, Y hint` summary. \
        Errors are always shown. \
        Only surface warnings from files you modified this session."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct CompileSelectedProjectArgs {
    /// If true, rebuilds the project from scratch. If false, performs an incremental compile.
    pub rebuild: Option<bool>,
    /// Project to compile: a numeric ID or a project name. A name matching several
    /// projects returns the candidate list instead of compiling. Takes precedence over project_id.
    pub project: Option<String>,
    /// Optional exact project ID to compile (alternative to `project`). If omitted and
    /// `project` is also omitted, the currently active project is compiled.
    pub project_id: Option<u64>,
    /// Show warning lines verbatim instead of suppressing them. Default: false.
    pub show_warnings: Option<bool>,
    /// Show hint lines verbatim instead of suppressing them. Default: false.
    pub show_hints: Option<bool>,
    /// Emit a per-file `<file>: X warn, Y hint` summary for any
    /// warnings/hints that were not shown verbatim. Default: false.
    pub summarize_diagnostics: Option<bool>,
}

#[macros::mcp_tool(
    name = "delphi_compile_file",
    description = "Compiles a Delphi project file (.dproj/.dpr/.dpk) from a path. \
        If the file already belongs to a managed project it is compiled as that project \
        (using its workspace compiler); if several projects share the file, the candidate \
        list is returned instead of compiling. Only a file owned by no project is compiled \
        ad-hoc (without adding it to a workspace), which is the main use of this tool. \
        A bare .dpr/.dpk without a .dproj is supported. \
        The compiler is selected by `compiler` (an exact key like \"12.0\" or a product name like \
        \"Delphi 12\"); if omitted, the newest installed compiler is used. \
        Call delphi_get_available_compilers to discover valid keys. \
        Optional config (\"Debug\"/\"Release\") and platform (\"Win32\"/\"Win64\") override the build. \
        Output filtering matches delphi_compile_project (banner stripped; warnings/hints suppressed by default)."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct CompileFileArgs {
    /// Absolute or relative path to the .dproj/.dpr/.dpk file to compile.
    pub file_path: String,
    /// Compiler configuration key (e.g. "12.0") or product name (e.g. "Delphi 12").
    /// If omitted, the newest installed compiler is used.
    pub compiler: Option<String>,
    /// Build configuration override, e.g. "Debug" or "Release". Optional.
    pub config: Option<String>,
    /// Target platform override, e.g. "Win32" or "Win64". Optional.
    pub platform: Option<String>,
    /// If true, rebuilds from scratch. If false/omitted, incremental compile.
    pub rebuild: Option<bool>,
    /// Show warning lines verbatim instead of suppressing them. Default: false.
    pub show_warnings: Option<bool>,
    /// Show hint lines verbatim instead of suppressing them. Default: false.
    pub show_hints: Option<bool>,
    /// Emit a per-file `<file>: X warn, Y hint` summary for suppressed diagnostics. Default: false.
    pub summarize_diagnostics: Option<bool>,
}

#[macros::mcp_tool(
    name = "delphi_run_project",
    description = "Runs a Delphi project's built executable directly. Does not compile — \
        the executable must already exist (call delphi_compile_project first if unsure). \
        Target it with `project`: either a numeric ID or a project name (e.g. \"be\"). \
        If a name matches several projects, the tool returns the list of candidates \
        (with their IDs, workspaces and paths) instead of running — re-call with the \
        chosen ID. `project_id` is also accepted for an exact numeric target. \
        Omit both to run the currently active project. \
        Use delphi_list_projects to discover names/IDs. Always match by project name from the user's request. \
        `args` overrides the project's saved Start Parameters (see the \"Set Start Parameters\" \
        project command in the VS Code extension) for this invocation only; omit it to use \
        whatever the project has saved. \
        The process is launched detached; this tool returns immediately without waiting for it to exit."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct RunProjectArgs {
    /// Project to run: a numeric ID or a project name. A name matching several
    /// projects returns the candidate list instead of running. Takes precedence over project_id.
    pub project: Option<String>,
    /// Optional exact project ID to run (alternative to `project`). If omitted and
    /// `project` is also omitted, the currently active project is run.
    pub project_id: Option<u64>,
    /// Command-line arguments passed to the executable, overriding the project's
    /// saved Start Parameters for this invocation only. Optional.
    pub args: Option<String>,
}

#[macros::mcp_tool(
    name = "delphi_run_file",
    description = "Runs an executable from a path. If the path is a .dproj/.dpr/.dpk, it must \
        already belong to a managed project — its stored executable is run, identical to \
        referencing the project by name — and a path shared by several projects returns the \
        candidate list instead of running. If the path is a .exe, it is launched directly, \
        bypassing project resolution entirely. Unlike delphi_compile_file, this never compiles \
        or assembles ad-hoc project state: the target executable must already exist. \
        `args` are command-line arguments for the process; for a project-file path they override \
        the project's saved Start Parameters for this invocation only. \
        The process is launched detached; this tool returns immediately without waiting for it to exit."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct RunFileArgs {
    /// Absolute or relative path to the .dproj/.dpr/.dpk/.exe to run.
    pub file_path: String,
    /// Command-line arguments passed to the executable. Optional.
    pub args: Option<String>,
}

#[macros::mcp_tool(
    name = "delphi_add_project",
    description = "Adds a Delphi project file (.dproj/.dpr/.dpk) to an existing workspace so it \
        becomes a managed project (listed by delphi_list_projects, compilable by delphi_compile_project). \
        The workspace is identified by name (e.g. \"Workspace 1\") or numeric id. \
        Use delphi_list_projects to see existing workspaces, or delphi_add_workspace to create one first."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct AddProjectArgs {
    /// Absolute or relative path to the .dproj/.dpr/.dpk file to add.
    pub file_path: String,
    /// Target workspace name (or numeric id).
    pub workspace: String,
}

#[macros::mcp_tool(
    name = "delphi_add_workspace",
    description = "Creates a new workspace bound to a compiler configuration. Projects added to \
        the workspace compile with this compiler. The compiler is selected by an exact key \
        (e.g. \"12.0\") or a product name (e.g. \"Delphi 12\"); call delphi_get_available_compilers \
        to discover valid keys."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct AddWorkspaceArgs {
    /// Name for the new workspace.
    pub name: String,
    /// Compiler key (e.g. "12.0") or product name (e.g. "Delphi 12").
    pub compiler: String,
}

#[macros::mcp_tool(
    name = "delphi_format_file",
    description = "Formats a Delphi source file (.pas / .dpr / .dpk) in-place using the DDK formatter. \
        The file is read from disk, reformatted, and written back to the same path. \
        Requires at least one Delphi compiler installation to be present. \
        Specify the encoding when the file is not UTF-8, e.g. \"windows-1252\" for ANSI or \"oem\" for the system OEM codepage."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct FormatFileArgs {
    /// Absolute or relative path to the Delphi source file to format.
    pub file_path: String,
    /// Encoding of the source file, e.g. "utf-8", "windows-1252", "oem".
    /// Defaults to "utf-8" when not specified.
    pub encoding: Option<String>,
}

rust_mcp_sdk::tool_box!(DdkTools, [
    GetDdkExtensionInfoArgs,
    GetEnvironmentInfoArgs,
    ListProjectsArgs,
    SelectProjectArgs,
    GetAvailableCompilersArgs,
    SetGroupProjectsCompilerArgs,
    CompileSelectedProjectArgs,
    CompileFileArgs,
    RunProjectArgs,
    RunFileArgs,
    AddProjectArgs,
    AddWorkspaceArgs,
    FormatFileArgs,
]);

// ---------------------------------------------------------------------------
// MCP server handler
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DdkMcpHandler;

#[async_trait]
impl ServerHandler for DdkMcpHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: DdkTools::tools(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let name = params.name.as_str();
        let args = Value::Object(params.arguments.clone().unwrap_or_default());
        let result_text = match name {
            "get_ddk_extension_info"          => get_ddk_extension_info().await,
            "delphi_get_environment_info"     => get_environment_info().await,
            "delphi_list_projects"            => list_projects().await,
            "delphi_select_project"           => select_project(&args).await,
            "delphi_get_available_compilers"  => get_available_compilers().await,
            "delphi_set_group_projects_compiler" => set_group_projects_compiler(&args).await,
            "delphi_compile_project"          => compile_project(&args).await,
            "delphi_compile_file"             => compile_file(&args).await,
            "delphi_run_project"              => run_project(&args).await,
            "delphi_run_file"                 => run_file(&args).await,
            "delphi_add_project"              => add_project(&args).await,
            "delphi_add_workspace"            => add_workspace(&args).await,
            "delphi_format_file"              => format_file(&args).await,
            _ => format!("Unknown tool: {name}"),
        };
        Ok(CallToolResult::text_content(vec![TextContent::from(result_text)]))
    }
}

async fn get_ddk_extension_info() -> String {
    README_CONTENT.to_string()
}

async fn get_environment_info() -> String {
    match commands::cmd_get_environment_info().await {
        Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_default(),
        Err(e) => format!("Error: {e}"),
    }
}

async fn list_projects() -> String {
    match commands::cmd_list_projects().await {
        Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_default(),
        Err(e) => format!("Error: {e}"),
    }
}

async fn select_project(args: &Value) -> String {
    let project_id = match args.get("project_id").and_then(|v| v.as_u64()) {
        Some(id) => id as usize,
        _ => return "Missing required parameter: project_id".to_string(),
    };
    match commands::cmd_select_project(project_id).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn get_available_compilers() -> String {
    match commands::cmd_list_compilers().await {
        Ok(compilers) => {
            if compilers.is_empty() {
                return "No compiler configurations available.".to_string();
            }
            serde_json::to_string_pretty(&compilers).unwrap_or_default()
        }
        Err(e) => format!("Error: {e}"),
    }
}

async fn set_group_projects_compiler(args: &Value) -> String {
    let compiler_key = match args.get("compiler").and_then(|v| v.as_str()) {
        Some(k) => k.to_string(),
        _ => return "Missing required parameter: compiler".to_string(),
    };
    match commands::cmd_set_group_compiler(compiler_key).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn compile_project(args: &Value) -> String {
    let rebuild = args.get("rebuild").and_then(|v| v.as_bool()).unwrap_or(false);
    let filter = CompileFilterOptions {
        trim_banners: true,
        show_warnings: args.get("show_warnings").and_then(|v| v.as_bool()).unwrap_or(false),
        show_hints: args.get("show_hints").and_then(|v| v.as_bool()).unwrap_or(false),
        summarize_diagnostics: args
            .get("summarize_diagnostics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    // `project` (name or id) takes precedence; fall back to a numeric `project_id`.
    let reference = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            args.get("project_id")
                .and_then(|v| v.as_u64())
                .map(|id| id.to_string())
        });
    match commands::cmd_compile_ref(rebuild, reference, filter, Vec::new()).await {
        Ok(commands::CompileOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::CompileOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn compile_file(args: &Value) -> String {
    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        _ => return "Missing required parameter: file_path".to_string(),
    };
    let compiler = args.get("compiler").and_then(|v| v.as_str()).map(|s| s.to_string());
    let config = args.get("config").and_then(|v| v.as_str()).map(|s| s.to_string());
    let platform = args.get("platform").and_then(|v| v.as_str()).map(|s| s.to_string());
    let rebuild = args.get("rebuild").and_then(|v| v.as_bool()).unwrap_or(false);
    let filter = CompileFilterOptions {
        trim_banners: true,
        show_warnings: args.get("show_warnings").and_then(|v| v.as_bool()).unwrap_or(false),
        show_hints: args.get("show_hints").and_then(|v| v.as_bool()).unwrap_or(false),
        summarize_diagnostics: args
            .get("summarize_diagnostics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    match commands::cmd_compile_file(file_path, compiler, config, platform, rebuild, filter, Vec::new()).await {
        Ok(commands::CompileOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::CompileOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn run_project(args: &Value) -> String {
    // `project` (name or id) takes precedence; fall back to a numeric `project_id`.
    let reference = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            args.get("project_id")
                .and_then(|v| v.as_u64())
                .map(|id| id.to_string())
        });
    let run_args = args.get("args").and_then(|v| v.as_str()).map(|s| s.to_string());
    match commands::cmd_run_ref(reference, run_args).await {
        Ok(commands::RunOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::RunOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn run_file(args: &Value) -> String {
    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        _ => return "Missing required parameter: file_path".to_string(),
    };
    let run_args = args.get("args").and_then(|v| v.as_str()).map(|s| s.to_string());
    match commands::cmd_run_path(file_path, run_args).await {
        Ok(commands::RunOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::RunOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn add_project(args: &Value) -> String {
    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        _ => return "Missing required parameter: file_path".to_string(),
    };
    let workspace = match args.get("workspace").and_then(|v| v.as_str()) {
        Some(w) => w.to_string(),
        _ => return "Missing required parameter: workspace".to_string(),
    };
    match commands::cmd_add_project(file_path, workspace).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn add_workspace(args: &Value) -> String {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        _ => return "Missing required parameter: name".to_string(),
    };
    let compiler = match args.get("compiler").and_then(|v| v.as_str()) {
        Some(c) => c.to_string(),
        _ => return "Missing required parameter: compiler".to_string(),
    };
    match commands::cmd_add_workspace(name, compiler).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn format_file(args: &Value) -> String {
    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        _ => return "Missing required parameter: file_path".to_string(),
    };
    let encoding = args
        .get("encoding")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    match commands::cmd_format_file(file_path, encoding).await {
        Ok(path) => format!("{path}"),
        Err(e) => format!("{e}"),
    }
}
