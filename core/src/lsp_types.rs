use tower_lsp::lsp_types::{MessageType, notification::Notification};
use serde::{Deserialize, Serialize};

use crate::projects::*;

pub enum EventDone {}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct EventDoneParams {
    pub event_id: String,
}

impl Notification for EventDone {
    type Params = EventDoneParams;
    const METHOD: &'static str = "notifications/event/done";
}

impl EventDone {
    pub async fn notify(client: &tower_lsp::Client, event_id: String) {
        client.send_notification::<EventDone>(EventDoneParams {
            event_id,
        }).await;
    }
    pub async fn notify_json(client: &tower_lsp::Client, json: &serde_json::Value) {
        if let Some(event_id_value) = json.get("event_id") {
            if let Some(event_id) = event_id_value.as_str() {
                client.send_notification::<EventDone>(EventDoneParams {
                    event_id: event_id.to_string(),
                }).await;
            }
        }
    }
    pub async fn notify_serde<T: Serialize>(client: &tower_lsp::Client, serializable: T) {
        let json = serde_json::to_value(serializable).unwrap_or(serde_json::Value::Null);
        Self::notify_json(client, &json).await;
    }
}

#[macro_export]
macro_rules! try_finish_event {
    ($client:expr, $serializable:expr) => {
        EventDone::notify_serde(&$client, $serializable).await;
        return Ok(());
    };
    ($client:expr, $serializable:expr, $ret:expr) => {
        EventDone::notify_serde(&$client, $serializable).await;
        return $ret;
    };
}

pub enum NotifyError {}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct NotifyErrorParams {
    pub message: String,
    pub event_id: Option<String>,
}

impl Notification for NotifyError {
    type Params = NotifyErrorParams;
    const METHOD: &'static str = "notifications/error";
}

impl NotifyError {
    pub async fn notify(client: &tower_lsp::Client, message: String, event_id: Option<String>) {
        client.send_notification::<NotifyError>(NotifyErrorParams {
            message,
            event_id,
        }).await;
    }

    pub async fn notify_json(client: &tower_lsp::Client, message: String, json: &serde_json::Value) {
        client.send_notification::<NotifyError>(NotifyErrorParams {
            message,
            event_id: json.get("event_id").and_then(|v| v.as_str().map(|s| s.to_string())),
        }).await;
    }
}

pub enum ProjectsUpdate {}

impl ProjectsUpdate {
    pub async fn notify(client: &tower_lsp::Client) {
        client.log_message(MessageType::INFO, "Projects updated").await;
        client.send_notification::<ProjectsUpdate>(ProjectsUpdateParams {
            projects: ProjectsData::new(),
        }).await;
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct ProjectsUpdateParams {
    pub projects: ProjectsData,
}

impl Notification for ProjectsUpdate {
    type Params = ProjectsUpdateParams;
    const METHOD: &'static str = "notifications/projects/update";
}

pub enum CompilersUpdate {}

impl CompilersUpdate {
    pub async fn notify(client: &tower_lsp::Client) {
        client.log_message(MessageType::INFO, "Compilers updated").await;
        client.send_notification::<CompilersUpdate>(CompilersUpdateParams {
            compilers: CompilerConfigurations::new(),
        }).await;
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct CompilersUpdateParams {
    pub compilers: CompilerConfigurations,
}

impl Notification for CompilersUpdate {
    type Params = CompilersUpdateParams;
    const METHOD: &'static str = "notifications/compilers/update";
}

pub enum CompilerProgress {}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum CompilerProgressParams {
    Start {
        lines: Vec<String>,
    },
    Stdout {
        line: String,
    },
    Stderr {
        line: String,
    },
    Completed {
        success: bool,
        cancelled: bool,
        code: i32,
        lines: Vec<String>,
    },
    SingleProjectStarted {
        project_id: usize,
        lines: Vec<String>,
    },
    SingleProjectCompleted {
        project_id: usize,
        success: bool,
        cancelled: bool,
        code: i32,
        lines: Vec<String>,
    },
}

impl Notification for CompilerProgress {
    type Params = CompilerProgressParams;
    const METHOD: &'static str = "notifications/compiler/progress";
}

use tokio::sync::broadcast;

static COMPILER_BROADCAST: std::sync::OnceLock<broadcast::Sender<CompilerProgressParams>> =
    std::sync::OnceLock::new();

fn broadcast_channel() -> &'static broadcast::Sender<CompilerProgressParams> {
    COMPILER_BROADCAST.get_or_init(|| broadcast::channel(512).0)
}

impl CompilerProgress {
    /// Subscribe to compiler progress events from within the same process.
    pub fn subscribe() -> broadcast::Receiver<CompilerProgressParams> {
        broadcast_channel().subscribe()
    }

    fn broadcast(params: &CompilerProgressParams) {
        // Ignore send errors (no active receivers is fine).
        let _ = broadcast_channel().send(params.clone());
    }

    pub async fn notify_start(client: Option<&tower_lsp::Client>, lines: Vec<String>) {
        let params = CompilerProgressParams::Start { lines };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }

    pub async fn notify_stdout(client: Option<&tower_lsp::Client>, line: String) {
        let params = CompilerProgressParams::Stdout { line };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }

    pub async fn notify_stderr(client: Option<&tower_lsp::Client>, line: String) {
        let params = CompilerProgressParams::Stderr { line };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }

    pub async fn notify_completed(client: Option<&tower_lsp::Client>, success: bool, cancelled: bool, code: i32, lines: Vec<String>) {
        let params = CompilerProgressParams::Completed { success, cancelled, code, lines };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }

    pub async fn notify_single_project_started(
        client: Option<&tower_lsp::Client>,
        project_id: usize,
        lines: Vec<String>
    ) {
        let params = CompilerProgressParams::SingleProjectStarted { project_id, lines };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }

    pub async fn notify_single_project_completed(
        client: Option<&tower_lsp::Client>,
        project_id: usize,
        success: bool,
        cancelled: bool,
        code: i32,
        lines: Vec<String>
    ) {
        let params = CompilerProgressParams::SingleProjectCompleted { project_id, success, cancelled, code, lines };
        Self::broadcast(&params);
        if let Some(client) = client {
            client.send_notification::<CompilerProgress>(params).await;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum CompileProjectParams {
    Project {
        project_id: usize,
        project_link_id: Option<usize>,
        rebuild: bool,
        /// Force the full debug artefact set (see `Compiler`'s debug-info
        /// handling); optional on the wire so older clients keep working.
        #[serde(default)]
        debug_info: bool,
        event_id: String,
    },
    AllInWorkspace {
        workspace_id: usize,
        rebuild: bool,
        #[serde(default)]
        debug_info: bool,
        event_id: String,
    },
    AllInGroupProject {
        rebuild: bool,
        #[serde(default)]
        debug_info: bool,
        event_id: String,
    },
    FromLink {
        project_link_id: usize,
        rebuild: bool,
        #[serde(default)]
        debug_info: bool,
        event_id: String,
    }
}

impl CompileProjectParams {
    /// Whether the request asks for the full debug artefact set regardless
    /// of the build configuration's own settings.
    pub fn debug_info(&self) -> bool {
        match self {
            Self::Project { debug_info, .. }
            | Self::AllInWorkspace { debug_info, .. }
            | Self::AllInGroupProject { debug_info, .. }
            | Self::FromLink { debug_info, .. } => *debug_info,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigurationFetchResponse {
    pub projects: ProjectsData,
    pub compilers: CompilerConfigurations,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CancelCompilationParams {}

/// Request params for `dproj/metadata` – asks the server for configuration
/// and platform information about a single project's .dproj file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DprojMetadataParams {
    pub project_id: usize,
}

/// Response for `dproj/metadata`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DprojMetadataResponse {
    /// All available build configurations (e.g. `["Debug","Release"]`).
    pub configurations: Vec<String>,
    /// All available target platforms (e.g. `["Win32","Win64"]`).
    pub platforms: Vec<String>,
    /// The effective active configuration (project override → dproj default).
    pub active_configuration: String,
    /// The effective active platform (project override → dproj default).
    pub active_platform: String,
}

/// Request params for `debug/target` – asks the server to describe a
/// project's debug target (see `ddk_core::debug_target`). Mirrors
/// `cmd_debug_target`: `project` is an id, a name or a project-file path,
/// `None` for the active project; `compiler` only matters for an ad-hoc path.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DebugTargetParams {
    pub project: Option<String>,
    pub compiler: Option<String>,
}

/// Request params for `delphilsp/generate` – asks the server to (re)write the
/// `.delphilsp.json` settings file consumed by Embarcadero's DelphiLSP
/// extension. All fields are optional and mirror `cmd_delphilsp_config`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DelphiLspGenerateParams {
    /// Project ID, project name, or path to a `.dproj`/`.dpr`/`.dpk`.
    /// Omit to use the currently active project.
    pub project: Option<String>,
    /// Compiler key or product name for a file that belongs to no workspace.
    pub compiler: Option<String>,
    /// Destination override; defaults to `<main source stem>.delphilsp.json`.
    pub out: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomDocumentFormat {
    /// The full document text. Range formatting still needs the whole file so
    /// the formatter has enough context; the selection is applied afterwards.
    pub content: String,
    /// The selection to format, as UTF-16 offsets (VS Code `offsetAt`
    /// semantics). `None` formats the whole document.
    pub range: Option<OffsetRange>,
}

/// A half-open selection expressed in UTF-16 code units (LSP / VS Code offset
/// semantics).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct OffsetRange {
    pub start: usize,
    pub end: usize,
}

/// A formatting edit returned to the client: replace `[start, end)` (UTF-16
/// offsets into the original document) with `new_text`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFormatEdit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

