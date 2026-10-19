use std::path::PathBuf;

use clap::Parser;
use mesh_talk_integrations::client::LocalClient;
use mesh_talk_mcp::MessagingServer;
use rmcp::{ServiceExt, transport::stdio};

#[derive(Parser)]
#[command(
    version,
    about = "Send messages through a private Mesh-Talk integration grant over MCP stdio"
)]
struct Arguments {
    /// Private integration client file exported by the running Mesh-Talk host.
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let client = LocalClient::from_file(&arguments.config)?;
    // stdout is exclusively owned by the SDK's MCP transport.
    let server = MessagingServer::new(client).serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
