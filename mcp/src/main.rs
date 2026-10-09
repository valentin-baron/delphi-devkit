//! Standalone MCP (Model Context Protocol) server for DDK, over STDIO.
//!
//! State is shared with ddk-server through RON files on disk; ddk-server's file
//! watcher picks up a changed project or compiler and pushes it to VS Code.

mod arguments;
mod handler;

use handler::DdkMcpHandler;
use ddk_core::projects::{ProjectsData, CompilerConfigurations};
use ddk_core::state::Stateful;

use rust_mcp_sdk::{
    McpServer, ToMcpServerHandler, StdioTransport, TransportOptions,
    mcp_server::{server_runtime, McpServerOptions, ServerRuntime},
    schema::{
        InitializeResult, Implementation, ServerCapabilities, ServerCapabilitiesTools,
        ProtocolVersion,
    },
    error::SdkResult,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> SdkResult<()> {
    ProjectsData::initialize().expect("Failed to initialize projects data");
    CompilerConfigurations::initialize().expect("Failed to initialize compiler configurations");

    let server_details = InitializeResult {
        server_info: Implementation {
            name: "ddk-mcp-server".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("DDK - Delphi Development Kit".into()),
            description: Some(
                "MCP server for managing Delphi projects, compilers, and running compilations."
                    .into(),
            ),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: Some(
            "Use these tools to query and manage Delphi projects and compiler configurations, \
             and to compile the currently active Delphi project."
                .into(),
        ),
        meta: None,
    };

    let transport = StdioTransport::new(TransportOptions::default())?;
    let handler = DdkMcpHandler;

    let server: Arc<ServerRuntime> = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
    });

    server.start().await
}
