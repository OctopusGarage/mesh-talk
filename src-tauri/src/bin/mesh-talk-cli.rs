use clap::Parser;
use mesh_talk::{api::Args, services::node_service::NodeService};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Parse CLI arguments
    let args = Args::parse();
    let config = args.into_config();
    let node_service = NodeService::new(config.name, config.port);

    // Wrap NodeService in Arc<TokioMutex<>> for sharing across threads
    let node_service = Arc::new(TokioMutex::new(node_service));

    // Run CLI
    mesh_talk::run_cli(node_service).await
}
