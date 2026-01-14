use tower_lsp::lsp_types::notification::Notification;
use serde::{Deserialize, Serialize};

use crate::{projects::{CompilerConfigurations, project_data::ProjectsData}};

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