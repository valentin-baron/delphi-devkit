use anyhow::Result;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;
use crate::{CompilersUpdate, ProjectsUpdate};
use crate::utils::FilePath;

use super::*;

fn create_watcher<F>(
    path: PathBuf,
    mut on_event: F,
) -> Result<RecommendedWatcher>
where
    F: FnMut(Event) + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(100);

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.blocking_send(res);
        },
        Config::default(),
    )?;

    watcher.watch(&path, RecursiveMode::NonRecursive)?;

    tokio::spawn(async move {
        while let Some(res) = rx.recv().await {
            if let Ok(event) = res {
                on_event(event);
            }
        }
    });

    Ok(watcher)
}

pub fn start_file_watchers(client: Client) -> Result<()> {
    // Watcher for ProjectsData
    let projects_client = client.clone();
    let _projects_watcher = create_watcher(
        ProjectsData::get_file_path(),
        move |event| {
            let client = projects_client.clone();
            tokio::spawn(async move {
                handle_projects_data_change(event, &client).await;
            });
        },
    )?;

    // Watcher for CompilerConfigurations
    let compiler_client = client.clone();
    let _compiler_watcher = create_watcher(
        CompilerConfigurations::get_file_path(),
        move |event| {
            let client = compiler_client.clone();
            tokio::spawn(async move {
                handle_compiler_config_change(event, &client).await;
            });
        },
    )?;

    // Keep watchers alive by storing them
    tokio::spawn(async move {
        let _keep_alive = (_projects_watcher, _compiler_watcher);
        // Wait forever to keep watchers alive
        std::future::pending::<()>().await;
    });

    Ok(())
}

async fn handle_projects_data_change(event: Event, client: &Client) {
    use notify::EventKind;

    match event.kind {
        EventKind::Modify(_) => {
            client.log_message(
                MessageType::INFO,
                "ProjectsData file modified".to_string()
            ).await;
        }
        EventKind::Create(_) => {
            client.log_message(
                MessageType::INFO,
                "ProjectsData file created".to_string()
            ).await;
        }
        EventKind::Remove(_) => {
            client.log_message(
                MessageType::WARNING,
                "ProjectsData file was deleted!".to_string()
            ).await;
        }
        _ => { return; }
    }
    CompilersUpdate::notify(client).await;
}

async fn handle_compiler_config_change(event: Event, client: &Client) {
    use notify::EventKind;

    match event.kind {
        EventKind::Modify(_) => {
            client.log_message(
                MessageType::INFO,
                "CompilerConfigurations file modified".to_string()
            ).await;
        }
        EventKind::Create(_) => {
            client.log_message(
                MessageType::INFO,
                "CompilerConfigurations file created".to_string()
            ).await;
        }
        EventKind::Remove(_) => {
            client.log_message(
                MessageType::WARNING,
                "CompilerConfigurations file was deleted!".to_string()
            ).await;
        }
        _ => { return; }
    }
    ProjectsUpdate::notify(client).await;
}