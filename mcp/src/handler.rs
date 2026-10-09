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

use crate::arguments::{ArgumentResult, Arguments};

static README_CONTENT: &str = include_str!("../../README.md");

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
    /// If true, forces the full debug artefact set a debugger needs (optimizations off,
    /// TD32 debug info, .rsm symbols, detailed .map) regardless of the build configuration.
    /// The dproj is never modified. Default: false.
    pub debug_info: Option<bool>,
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
    /// If true, forces the full debug artefact set (optimizations off, TD32 debug info,
    /// .rsm symbols, detailed .map) regardless of the build configuration. Default: false.
    pub debug_info: Option<bool>,
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

#[macros::mcp_tool(
    name = "delphi_generate_delphilsp_config",
    description = "Generates the `<project>.delphilsp.json` settings file that Embarcadero's \
        DelphiLSP VS Code extension needs for Delphi code insight (completion, go-to-definition), \
        so it works without ever opening the RAD Studio IDE. \
        The file is reconstructed from the project's .dproj, the compiler installation's rsvars.bat, \
        and the IDE's global Library Path + environment-variable overrides from the registry. \
        Target it with `project`: a numeric ID, a project name, or a path to a .dproj/.dpr/.dpk. \
        A name matching several projects returns the candidate list instead of writing anything. \
        Omit `project` to use the currently active project. \
        A path that belongs to no workspace is handled ad-hoc: pick its compiler with `compiler` \
        (an exact key like \"12.0\" or a product name like \"Delphi 12\"; default: newest installed). \
        `out` overrides the destination, which is otherwise `<main source stem>.delphilsp.json` \
        next to the project. Re-run it after changing the project's search paths, defines, \
        configuration or platform."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct GenerateDelphiLspConfigArgs {
    /// Project to describe: a numeric ID, a project name, or a path to a
    /// .dproj/.dpr/.dpk. Omit to use the currently active project.
    pub project: Option<String>,
    /// Compiler key (e.g. "12.0") or product name (e.g. "Delphi 12"), used only
    /// for a file path that belongs to no workspace. Optional.
    pub compiler: Option<String>,
    /// Write the settings file here instead of next to the project's main source. Optional.
    pub out: Option<String>,
}

#[macros::mcp_tool(
    name = "delphi_get_debug_target",
    description = "Describes what debugging a Delphi project means, independently of any debugger: \
        the executable to launch or attach to (the program itself, or the Host Application that loads \
        a package/DLL), the .map/.rsm symbol files next to it, the project's own .bpl/.dll module with \
        its symbols and .dcp, the source search paths (project directory, dproj unit/include paths, the \
        IDE's Library and Browsing Paths, the compiler's source tree), the run arguments (dproj Run \
        Parameters fused with the saved Start Parameters) and config/platform/bitness. \
        Every path in the answer except `executable` is a file that exists: a symbol file, module or \
        .dcp that is missing, empty or left by another build is null. `warnings` lists what will \
        degrade or break a session (missing or stale artefacts, a platform the project does not \
        enable, a value depending on an undefined $(NAME), an input that could not be read); \
        `notes` lists what is merely worth knowing. An empty `warnings` list means the project is \
        ready to debug. \
        Use it to build a debugger launch or attach configuration, or to check that readiness. \
        Target it with `project`: a numeric ID, a project name, or a path to a .dproj/.dpr/.dpk \
        (`project_id` is also accepted for an exact numeric target). \
        A name matching several projects returns the candidate list instead. \
        Omit both to describe the currently active project. \
        `config`/`platform` describe that configuration and platform instead of the project's \
        active ones — the same overrides the compile tools take — with the executable and the \
        host discovered for that build; nothing is persisted. \
        `compiler` (an exact key like \"12.0\" or a product name like \"Delphi 12\"; default: newest \
        installed) picks the compiler of a project that has none of its own: a path that belongs to \
        no workspace, described ad-hoc, or a project linked to no workspace. It is ignored, with a \
        note, for a project that builds with its workspace's compiler. \
        Nothing is written or compiled: compile with debug_info first if the warnings ask for it."
)]
#[derive(Debug, Deserialize, Serialize, macros::JsonSchema)]
pub struct GetDebugTargetArgs {
    /// Project to describe: a numeric ID, a project name, or a path to a
    /// .dproj/.dpr/.dpk. Omit to use the currently active project. Takes
    /// precedence over project_id.
    pub project: Option<String>,
    /// Numeric project ID, as an alternative to `project`.
    pub project_id: Option<u64>,
    /// Compiler key (e.g. "12.0") or product name (e.g. "Delphi 12"), used only
    /// for a project without a compiler of its own: a file path that belongs
    /// to no workspace, or a project linked to no workspace. Optional.
    pub compiler: Option<String>,
    /// Build configuration to describe (e.g. "Debug", "Release") instead of
    /// the project's active one. Optional; nothing is persisted.
    pub config: Option<String>,
    /// Target platform to describe (e.g. "Win32", "Win64") instead of the
    /// project's active one. Optional; nothing is persisted.
    pub platform: Option<String>,
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
    GenerateDelphiLspConfigArgs,
    GetDebugTargetArgs,
]);

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
        // Returned as an error result, not as text: a `CallToolResult` with
        // no `isError` is a success on the wire, so a client branching on it
        // would read "the call was fine" about a call that did nothing.
        if let Err(message) = reject_unknown_arguments(name, &args) {
            return Ok(CallToolResult::with_error(CallToolError::from_message(message)));
        }
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
            "delphi_generate_delphilsp_config" => generate_delphilsp_config(&args).await,
            "delphi_get_debug_target"         => get_debug_target(&args).await,
            // Also an error result: a name the server does not serve is a
            // protocol failure, not an answer.
            _ => return Ok(CallToolResult::with_error(CallToolError::unknown_tool(name))),
        };
        Ok(CallToolResult::text_content(vec![TextContent::from(result_text)]))
    }
}

/// Fails when the call carries an argument the tool does not advertise: a
/// misspelled `rebiuld` would otherwise read as a silent `false`. The accepted
/// names come from the tool's own published schema, so this cannot disagree
/// with what the client was told.
fn reject_unknown_arguments(tool: &str, args: &Value) -> Result<(), String> {
    let Some(given) = args.as_object().filter(|given| !given.is_empty()) else {
        return Ok(());
    };
    let Some(definition) = DdkTools::tools().into_iter().find(|definition| definition.name == tool) else {
        return Ok(());
    };
    let Some(accepted) = definition.input_schema.properties.as_ref() else {
        return Ok(());
    };
    let unknown: Vec<&str> = given.keys().map(String::as_str).filter(|name| !accepted.contains_key(*name)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let mut names: Vec<&str> = accepted.keys().map(String::as_str).collect();
    names.sort_unstable();
    let takes = match names.is_empty() {
        true => "It takes none.".to_string(),
        false => format!("It takes: {}.", names.join(", ")),
    };
    Err(format!(
        "Unknown parameter{} for {tool}: {}. {takes}",
        if unknown.len() == 1 { "" } else { "s" },
        unknown.join(", ")
    ))
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

/// The output filter both compile tools take.
fn compile_filter(arguments: &Arguments) -> ArgumentResult<CompileFilterOptions> {
    Ok(CompileFilterOptions {
        trim_banners: true,
        show_warnings: arguments.flag("show_warnings")?,
        show_hints: arguments.flag("show_hints")?,
        summarize_diagnostics: arguments.flag("summarize_diagnostics")?,
    })
}

async fn select_project(args: &Value) -> String {
    let project_id = match Arguments::new(args).required_number("project_id") {
        Ok(id) => id as usize,
        Err(message) => return message,
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
    let compiler_key = match Arguments::new(args).required_text("compiler") {
        Ok(key) => key,
        Err(message) => return message,
    };
    match commands::cmd_set_group_compiler(compiler_key).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn compile_project(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> {
        Ok((arguments.flag("rebuild")?, arguments.flag("debug_info")?, compile_filter(&arguments)?, arguments.project_reference()?))
    })();
    let (rebuild, debug_info, filter, reference) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_compile_ref(rebuild, debug_info, reference, filter, Vec::new()).await {
        Ok(commands::CompileOrAmbiguity::Output(output)) => {
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string())
        }
        Ok(commands::CompileOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn compile_file(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> {
        Ok((
            arguments.required_text("file_path")?,
            arguments.text("compiler")?,
            arguments.text("config")?,
            arguments.text("platform")?,
            arguments.flag("rebuild")?,
            arguments.flag("debug_info")?,
            compile_filter(&arguments)?,
        ))
    })();
    let (file_path, compiler, config, platform, rebuild, debug_info, filter) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_compile_file(file_path, compiler, config, platform, rebuild, debug_info, filter, Vec::new()).await {
        Ok(commands::CompileOrAmbiguity::Output(output)) => {
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string())
        }
        Ok(commands::CompileOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn run_project(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.project_reference()?, arguments.text("args")?)) })();
    let (reference, run_args) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_run_ref(reference, run_args).await {
        Ok(commands::RunOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::RunOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn run_file(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.required_text("file_path")?, arguments.text("args")?)) })();
    let (file_path, run_args) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_run_path(file_path, run_args).await {
        Ok(commands::RunOrAmbiguity::Output(output)) => output.to_string(),
        Ok(commands::RunOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn add_project(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.required_text("file_path")?, arguments.required_text("workspace")?)) })();
    let (file_path, workspace) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_add_project(file_path, workspace).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn add_workspace(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.required_text("name")?, arguments.required_text("compiler")?)) })();
    let (name, compiler) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_add_workspace(name, compiler).await {
        Ok(result) => result.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn generate_delphilsp_config(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.project_reference()?, arguments.text("compiler")?, arguments.text("out")?)) })();
    let (project, compiler, out) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_delphilsp_config(project, compiler, out).await {
        Ok(commands::DelphiLspOrAmbiguity::Output(result)) => result.to_string(),
        Ok(commands::DelphiLspOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

async fn format_file(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> { Ok((arguments.required_text("file_path")?, arguments.text("encoding")?)) })();
    let (file_path, encoding) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_format_file(file_path, encoding).await {
        Ok(path) => format!("{path}"),
        Err(e) => format!("{e}"),
    }
}

async fn get_debug_target(args: &Value) -> String {
    let arguments = Arguments::new(args);
    let parsed = (|| -> ArgumentResult<_> {
        Ok((arguments.project_reference()?, arguments.text("compiler")?, arguments.text("config")?, arguments.text("platform")?))
    })();
    let (project, compiler, config, platform) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return message,
    };
    match commands::cmd_debug_target(project, compiler, config, platform).await {
        Ok(commands::DebugTargetOrAmbiguity::Target(target)) => {
            serde_json::to_string_pretty(&target).unwrap_or_else(|_| target.to_string())
        }
        Ok(commands::DebugTargetOrAmbiguity::Ambiguity(amb)) => amb.to_string(),
        Err(e) => format!("{e}"),
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_misspelled_parameter_is_named_instead_of_ignored() {
        let message = reject_unknown_arguments("delphi_compile_project", &json!({ "rebiuld": true }))
            .expect_err("a misspelling the tool does not advertise");

        assert!(message.contains("rebiuld"), "{message}");
        assert!(message.contains("rebuild"), "the accepted names are offered: {message}");
    }

    #[test]
    fn the_parameters_a_tool_advertises_are_accepted() {
        let call = json!({ "project": "be", "rebuild": true, "debug_info": true });

        assert_eq!(reject_unknown_arguments("delphi_compile_project", &call), Ok(()));
    }

    /// A tool that takes nothing still has to reject something, and say so
    /// without trailing an empty list.
    #[test]
    fn a_tool_that_takes_no_parameters_rejects_one_it_is_given() {
        let message = reject_unknown_arguments("delphi_list_projects", &json!({ "bogus": 1 }))
            .expect_err("a tool with no parameters accepts none");

        assert!(message.contains("bogus"), "{message}");
        assert!(message.contains("takes none"), "{message}");
    }

    /// An unknown tool is answered by the dispatcher; checking its arguments
    /// here would hide that.
    #[test]
    fn an_unknown_tool_is_left_to_the_dispatcher() {
        assert_eq!(reject_unknown_arguments("delphi_no_such_tool", &json!({ "whatever": 1 })), Ok(()));
    }
}
