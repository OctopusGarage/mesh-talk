//! Opt-in physical-network acceptance host over the public SDK. Uses only
//! disposable profiles; it never opens or modifies the desktop application's data.
//! Line protocol is for test orchestration, not a public application protocol.
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::Parser;
use mesh_talk_core::eventlog::{ConversationId, EventId};
use mesh_talk_core::node::{NodeRuntime, RuntimeConfig, RuntimeEvents, SyncCategory};
use std::{io::Write, path::PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    data: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long)]
    discovery_port: u16,
}

fn emit(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
    let _ = std::io::stdout().flush();
}

async fn start(args: &Args) -> Result<NodeRuntime, Box<dyn std::error::Error>> {
    let node = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: &args.data,
            account_id: "physical-acceptance",
            display_name: &args.name,
            password: "disposable-test-profile",
            discovery_port: args.discovery_port,
            post_office_enabled: false,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|dm| emit(format!("dm {} {}", dm.from, STANDARD.encode(dm.text)))),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|file| {
                emit(format!(
                    "file-offer {} {} {}",
                    hex::encode(file.file_conv.as_bytes()),
                    file.from,
                    file.size
                ))
            }),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|signal| {
                emit(format!(
                    "signal {} {}",
                    signal.from,
                    STANDARD.encode(signal.payload)
                ))
            }),
        },
    )
    .await?;
    emit(format!(
        "ready {} {} {} {}",
        node.user_id(),
        node.account_id(),
        node.listen_tcp_port(),
        node.discovery_port()
    ));
    Ok(node)
}

fn id_bytes(value: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    hex::decode(value)?
        .try_into()
        .map_err(|_| "expected 32-byte event ID".into())
}

async fn command(node: &NodeRuntime, line: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut words = line.split_whitespace();
    let operation = words.next().ok_or("empty command")?;
    let mut argument = || words.next().ok_or("missing command argument");
    match operation {
        "probe" => node.probe_endpoint(argument()?.parse()?).await?,
        "peers" => {
            for peer in node.peers() {
                emit(format!(
                    "peer {} {} {}",
                    peer.public.user_id(),
                    peer.account_id.as_deref().unwrap_or("-"),
                    peer.addr
                ));
            }
        }
        "signal" => {
            let target = argument()?;
            let payload = STANDARD.decode(argument()?)?;
            node.handle().send_call_signal(target, &payload).await?;
        }
        "link-code" => emit(format!("link-code {}", node.start_linking())),
        "link" => {
            let target = argument()?;
            let code = argument()?;
            emit(format!("linked {}", node.link_device(target, code).await?));
        }
        "account-msg" => {
            let target = argument()?;
            let payload = STANDARD.decode(argument()?)?;
            node.send_to_account(target, &payload, None).await?;
        }
        "history" => {
            for entry in node.account_history(argument()?, 100) {
                emit(format!(
                    "entry {} {}",
                    hex::encode(entry.id.as_bytes()),
                    STANDARD.encode(entry.text)
                ));
            }
        }
        "relay" => node.handle().set_post_office_enabled(argument()? == "on"),
        "bridge" => node
            .handle()
            .set_bridge_discovery_enabled(argument()? == "on"),
        "prefer" => node
            .handle()
            .set_bridge_hub_id(Some(argument()?))
            .map_err(std::io::Error::other)?,
        "enqueue" => {
            let target = argument()?;
            let payload = STANDARD.decode(argument()?)?;
            let id = node
                .handle()
                .enqueue_to_account(target, &payload, None)
                .await?;
            emit(format!("queued {}", hex::encode(id.as_bytes())));
        }
        "delivery-status" => emit(format!(
            "delivery-status {:?}",
            node.handle()
                .delivery_status(EventId::new(id_bytes(argument()?)?))
        )),
        "enqueue-file" => {
            let target = argument()?;
            let path = PathBuf::from(String::from_utf8(STANDARD.decode(argument()?)?)?);
            let (id, file) = node
                .handle()
                .enqueue_file_to_account(target, &path, mesh_talk_core::file::FileKind::File)
                .await?;
            emit(format!(
                "queued-file {} {}",
                hex::encode(id.as_bytes()),
                hex::encode(file.as_bytes())
            ));
        }
        "approve" => node
            .handle()
            .decide_attachment(ConversationId::new(id_bytes(argument()?)?), true)?,
        "file-ready" => emit(format!(
            "file-ready {}",
            node.handle()
                .file_ready_to_save(ConversationId::new(id_bytes(argument()?)?))
        )),
        "save-file" => {
            let file = ConversationId::new(id_bytes(argument()?)?);
            let path = PathBuf::from(String::from_utf8(STANDARD.decode(argument()?)?)?);
            node.handle().save_file(file, &path)?;
            emit(format!("saved {}", hex::encode(file.as_bytes())));
        }
        "file-sent" => emit(format!(
            "file-sent {}",
            node.handle()
                .outgoing_file_progress(EventId::new(id_bytes(argument()?)?))
                .is_some_and(|progress| progress.delivered)
        )),
        "history-sync" => {
            let enabled = match argument()? {
                "on" => true,
                "off" => false,
                _ => return Err("expected on or off".into()),
            };
            node.handle()
                .set_device_sync_category(SyncCategory::History, enabled)?;
        }
        _ => return Err("unknown acceptance command".into()),
    }
    emit(format!("ok {operation}"));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.data)?;
    let mut node = start(&args).await?;
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = input.next_line().await? {
        if line.len() > 2 * 1024 * 1024 {
            emit("error command-too-large");
            continue;
        }
        if line == "quit" || line == "/quit" {
            break;
        }
        if line == "restart" {
            node.stop().await;
            node = start(&args).await?;
        } else if let Err(error) = command(&node, &line).await {
            emit(format!("error {}", STANDARD.encode(error.to_string())));
        }
    }
    node.stop().await;
    Ok(())
}
