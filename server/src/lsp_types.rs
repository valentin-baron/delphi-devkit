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
    const METHOD: &'static str = "$/notifications/event/done";
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
}

pub enum NotifyError {}

#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct NotifyErrorParams {
    pub message: String,
    pub event_id: Option<String>,
}

impl Notification for NotifyError {
    type Params = NotifyErrorParams;
    const METHOD: &'static str = "$/notifications/error";
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
    const METHOD: &'static str = "$/notifications/projects/update";
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
    const METHOD: &'static str = "$/notifications/compilers/update";
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
        code: isize,
        lines: Vec<String>,
    },
    SingleProjectCompleted {
        project_id: usize,
        success: bool,
        code: isize,
        lines: Vec<String>,
    },
}

impl Notification for CompilerProgress {
    type Params = CompilerProgressParams;
    const METHOD: &'static str = "$/notifications/compiler/progress";
}

impl CompilerProgress {
    pub async fn notify_start(client: &tower_lsp::Client, lines: Vec<String>) {
        client.send_notification::<CompilerProgress>(CompilerProgressParams::Start {
            lines,
        }).await;
    }

    pub async fn notify_stdout(client: &tower_lsp::Client, line: String) {
        client.send_notification::<CompilerProgress>(CompilerProgressParams::Stdout {
            line,
        }).await;
    }

    pub async fn notify_stderr(client: &tower_lsp::Client, line: String) {
        client.send_notification::<CompilerProgress>(CompilerProgressParams::Stderr {
            line,
        }).await;
    }

    pub async fn notify_completed(client: &tower_lsp::Client, success: bool, code: isize, lines: Vec<String>) {
        client.send_notification::<CompilerProgress>(CompilerProgressParams::Completed {
            success,
            code,
            lines,
        }).await;
    }

    pub async fn notify_single_project_completed(client: &tower_lsp::Client, project_id: usize, success: bool, code: isize, lines: Vec<String>) {
        client.send_notification::<CompilerProgress>(CompilerProgressParams::SingleProjectCompleted {
            project_id,
            success,
            code,
            lines,
        }).await;
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum CompileProjectParams {
    Project {
        project_id: usize,
        project_link_id: Option<usize>,
        rebuild: bool,
    },
    AllInWorkspace {
        workspace_id: usize,
        rebuild: bool,
    },
    AllInGroupProject {
        rebuild: bool,
    },
    FromLink {
        project_link_id: usize,
        rebuild: bool,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigurationFetchResponse {
    pub projects: ProjectsData,
    pub compilers: CompilerConfigurations,
}