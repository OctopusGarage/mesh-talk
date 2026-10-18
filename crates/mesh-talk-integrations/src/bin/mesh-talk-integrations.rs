//! Operate a separate bot identity or a scoped desktop-local integration.
use clap::{Parser, Subcommand};
use mesh_talk_core::node::{NodeRuntime, RuntimeConfig, RuntimeEvents, RuntimeNetworkOptions};
use mesh_talk_integrations::{
    Destination, IntegrationError, IntegrationService,
    client::{ClientConfig, EndpointManifest, LocalClient, endpoint_reference},
    host::HeadlessHost,
    ipc::LocalServer,
    policy::{GrantRegistry, new_token, now_ms},
    private_files::{read_private_json, replace_private_json, write_private_json},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Parser)]
#[command(about = "Run private Mesh-Talk bot profiles and administer scoped agent access")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve(ServeOptions),
    AdminContext {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    GrantCreate {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        client_id: String,
        /// Repeat for each exact account:<fingerprint> or channel:<fingerprint>.
        #[arg(long, required = true)]
        destination: Vec<String>,
        #[arg(long, default_value_t = 86400)]
        expires_in_seconds: u64,
        #[arg(long)]
        describe: bool,
        #[arg(long)]
        out: PathBuf,
    },
    GrantList {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    AuditList {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    GrantRevoke {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        grant_id: String,
    },
    Send {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        destination: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long)]
        expires_at_ms: Option<u64>,
    },
    Status {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        idempotency_key: String,
    },
    Context {
        #[arg(long)]
        config: PathBuf,
    },
    ListConversations {
        #[arg(long)]
        config: PathBuf,
    },
    AcceptInvite {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        channel_id: String,
    },
}
#[derive(clap::Args)]
struct ServeOptions {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long, default_value = "bot")]
    profile: String,
    /// Protected JSON file containing {"password":"..."}.
    #[arg(long)]
    password_file: PathBuf,
    #[arg(long, default_value = "Bot")]
    name: String,
    #[arg(long, default_value_t = 47474)]
    discovery_port: u16,
    /// Opt in to discovered post offices for offline delivery.
    #[arg(long)]
    use_post_office: bool,
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    /// A new private client configuration; existing files are never overwritten.
    #[arg(long)]
    admin_config: Option<PathBuf>,
}
fn invalid(message: &str) -> IntegrationError {
    IntegrationError::new("INVALID_INPUT", message)
}
fn destination(value: &str) -> Result<Destination, IntegrationError> {
    let (kind, id) = value
        .split_once(':')
        .ok_or_else(|| invalid("Use account:<fingerprint> or channel:<fingerprint>"))?;
    let result = match kind {
        "account" => Destination::Account { id: id.into() },
        "channel" => Destination::Channel { id: id.into() },
        _ => return Err(invalid("Unknown destination kind")),
    };
    result.validate()?;
    Ok(result)
}
fn profile_name(value: &str) -> Result<(), IntegrationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid(
            "Profile names require 1–64 ASCII letters, digits, hyphens or underscores",
        ));
    }
    Ok(())
}
fn private_directory(path: &Path) -> Result<(), IntegrationError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(invalid("Profile directory must be a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o700 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(invalid(
                "Profile directory must be owned by this user with mode 0700",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(invalid("Bot directories cannot be reparse points"));
        }
    }
    Ok(())
}
#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BotRootMarker {
    kind: String,
    version: u32,
}
fn reserve_bot_root(path: &Path) -> Result<(), IntegrationError> {
    private_directory(path)?;
    let marker_path = path.join("bot-host.json");
    if marker_path.exists() {
        let marker: BotRootMarker = read_private_json(&marker_path)?;
        if marker.kind != "mesh-talk-headless-root" || marker.version != 1 {
            return Err(invalid("Unrecognized bot data directory"));
        }
    } else {
        if std::fs::read_dir(path)?.next().is_some() {
            return Err(invalid(
                "Choose an empty dedicated bot data directory; existing desktop or node profiles cannot be adopted",
            ));
        }
        write_private_json(
            &marker_path,
            &BotRootMarker {
                kind: "mesh-talk-headless-root".into(),
                version: 1,
            },
        )?;
    }
    Ok(())
}
fn lock_profile(path: &Path) -> Result<File, IntegrationError> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path.join("host.lock"))?;
    file.try_lock().map_err(|_| {
        IntegrationError::new("PROFILE_BUSY", "Another process owns this bot profile")
    })?;
    Ok(file)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordFile {
    password: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse().command).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
async fn run(command: Command) -> Result<(), IntegrationError> {
    let value = match command {
        Command::Serve(options) => return serve(options).await,
        Command::GrantCreate {
            config,
            client_id,
            destination: destinations,
            expires_in_seconds,
            describe,
            out,
        } => {
            if out.exists() {
                return Err(invalid("Client export already exists"));
            }
            let config: ClientConfig = read_private_json(&config)?;
            let endpoint = config.endpoint.clone();
            let destinations: Vec<_> = destinations
                .iter()
                .map(|value| destination(value))
                .collect::<Result<_, _>>()?;
            let duration = expires_in_seconds
                .checked_mul(1000)
                .ok_or_else(|| invalid("Expiry is too large"))?;
            let expires_at_ms = now_ms()
                .checked_add(duration)
                .ok_or_else(|| invalid("Expiry is too large"))?;
            let client = LocalClient::new(config);
            let mut value = client
                .call(
                    "grant_create",
                    json!({
                        "client_id": client_id, "destinations": destinations,
                        "describe": describe, "expires_at_ms": expires_at_ms
                    }),
                )
                .await?;
            let token = value
                .get("token")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invalid("Invalid grant response"))?;
            if let Err(error) = write_private_json(&out, &ClientConfig { endpoint, token }) {
                if let Some(grant_id) = value.pointer("/grant/id").and_then(Value::as_str) {
                    let _ = client
                        .call("grant_revoke", json!({"grant_id": grant_id}))
                        .await;
                }
                return Err(error);
            }
            if let Some(object) = value.as_object_mut() {
                object.remove("token");
            }
            value
        }
        Command::AdminContext {
            config,
            offset,
            limit,
        } => {
            call(
                &config,
                "admin_context",
                json!({"offset":offset,"limit":limit}),
            )
            .await?
        }
        Command::GrantList {
            config,
            offset,
            limit,
        } => {
            call(
                &config,
                "grant_list",
                json!({"offset":offset,"limit":limit}),
            )
            .await?
        }
        Command::AuditList {
            config,
            offset,
            limit,
        } => {
            call(
                &config,
                "audit_list",
                json!({"offset":offset,"limit":limit}),
            )
            .await?
        }
        Command::GrantRevoke { config, grant_id } => {
            call(&config, "grant_revoke", json!({"grant_id": grant_id})).await?
        }
        Command::Send {
            config,
            destination: target,
            text,
            idempotency_key,
            reply_to,
            expires_at_ms,
        } => {
            let params = json!({"destination": destination(&target)?, "text": text,
                "idempotency_key": idempotency_key, "reply_to": reply_to, "expires_at_ms": expires_at_ms});
            call(&config, "send_message", params).await?
        }
        Command::Status {
            config,
            idempotency_key,
        } => {
            call(
                &config,
                "get_send_result",
                json!({"idempotency_key": idempotency_key}),
            )
            .await?
        }
        Command::Context { config } => call(&config, "context", json!({})).await?,
        Command::ListConversations { config } => {
            call(&config, "list_conversations", json!({})).await?
        }
        Command::AcceptInvite { config, channel_id } => {
            call(&config, "accept_invite", json!({"channel_id": channel_id})).await?
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&value)
            .map_err(|_| invalid("Cannot encode operation result"))?
    );
    Ok(())
}

async fn call(config: &Path, method: &str, params: Value) -> Result<Value, IntegrationError> {
    LocalClient::from_file(config)?.call(method, params).await
}

async fn serve(options: ServeOptions) -> Result<(), IntegrationError> {
    let ServeOptions {
        data_dir,
        profile,
        password_file,
        name,
        discovery_port,
        use_post_office,
        runtime_dir,
        admin_config,
    } = options;
    profile_name(&profile)?;
    reserve_bot_root(&data_dir)?;
    let profile_dir = data_dir.join("accounts").join(&profile);
    private_directory(&profile_dir)?;
    let _profile_lock = lock_profile(&profile_dir)?;
    let endpoint_manifest = std::path::absolute(profile_dir.join("integration-endpoint.json"))?;
    let endpoint_ref = endpoint_reference(&endpoint_manifest)?;
    let password: PasswordFile = read_private_json(&password_file)?;
    if password.password.is_empty() {
        return Err(invalid("Bot profile password must not be empty"));
    }
    let temporary_runtime = if runtime_dir.is_none() {
        Some(default_runtime_directory()?)
    } else {
        None
    };
    let runtime_dir = runtime_dir
        .as_deref()
        .or_else(|| temporary_runtime.as_ref().map(|dir| dir.path()))
        .ok_or_else(|| invalid("Missing runtime directory"))?;
    let server = LocalServer::bind(runtime_dir).await?;
    let runtime = NodeRuntime::start_configured_guarded_with_network(
        RuntimeConfig {
            base_dir: &data_dir,
            account_id: &profile,
            display_name: &name,
            password: &password.password,
            discovery_port,
            post_office_enabled: false,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
        RuntimeNetworkOptions {
            post_office_send_enabled: use_post_office,
            post_office_receive_enabled: use_post_office,
            physical_lan_only: false,
        },
        |launch| Ok(launch(&name)),
    )
    .await
    .map_err(|_| {
        IntegrationError::new("STARTUP_FAILED", "Cannot open or start this bot profile")
    })?;
    let host = Arc::new(HeadlessHost::new(runtime.handle()));
    let registry = match GrantRegistry::open(
        &profile_dir.join("integration-grants.log"),
        &password.password,
        runtime.account_id(),
    ) {
        Ok(registry) => registry,
        Err(error) => {
            host.retire().await;
            runtime.stop().await;
            return Err(error);
        }
    };
    if let Err(error) = replace_private_json(
        &endpoint_manifest,
        &EndpointManifest {
            endpoint: server.endpoint(),
        },
    ) {
        host.retire().await;
        runtime.stop().await;
        return Err(error);
    }
    let token = new_token();
    let service = Arc::new(IntegrationService::new(host.clone(), registry, &token));
    let admin_config = admin_config.unwrap_or_else(|| {
        profile_dir.join(format!("admin-{}.json", uuid::Uuid::new_v4().simple()))
    });
    if let Err(error) = write_private_json(
        &admin_config,
        &ClientConfig {
            endpoint: endpoint_ref,
            token,
        },
    ) {
        let _ = std::fs::remove_file(&endpoint_manifest);
        host.retire().await;
        runtime.stop().await;
        return Err(error);
    }
    println!(
        "{}",
        json!({"account_id":runtime.account_id(),"device_id":runtime.user_id(),"admin_config":admin_config,"status":"ready"})
    );
    let result = server
        .run_until(service, async {
            shutdown_signal().await;
        })
        .await;
    host.retire().await;
    runtime.stop().await;
    let _ = std::fs::remove_file(&admin_config);
    let _ = std::fs::remove_file(&endpoint_manifest);
    result
}

fn default_runtime_directory() -> Result<tempfile::TempDir, IntegrationError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("mt-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    Ok(builder.tempdir()?)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn default_runtime_directory_can_bind_a_private_listener() {
        use std::os::unix::fs::PermissionsExt;
        let directory = default_runtime_directory().unwrap();
        assert_eq!(
            directory.path().metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _listener = LocalServer::bind(directory.path()).await.unwrap();
    }

    #[test]
    fn profile_rejects_path_escape() {
        for value in ["", "..", "../bot", "/bot", "a/b", "a\\b"] {
            assert!(profile_name(value).is_err());
        }
        assert!(profile_name("build-bot_1").is_ok());
    }
    #[test]
    fn destinations_require_exact_fingerprints() {
        assert!(destination(&format!("account:{}", "a".repeat(32))).is_ok());
        assert!(destination(&format!("channel:{}", "b".repeat(64))).is_ok());
        assert!(destination("channel:abcd").is_err());
        assert!(destination("user:alice").is_err());
    }
    #[test]
    fn profile_lock_is_exclusive_and_reusable() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock_profile(dir.path()).unwrap();
        assert!(lock_profile(dir.path()).is_err());
        drop(first);
        assert!(lock_profile(dir.path()).is_ok());
    }
    #[test]
    fn command_requires_retry_key() {
        assert!(
            Cli::try_parse_from([
                "mesh-talk-integrations",
                "send",
                "--config",
                "client.json",
                "--destination",
                "account:alice",
                "--text",
                "hello"
            ])
            .is_err()
        );
    }
    #[tokio::test]
    async fn grant_command_exports_only_scoped_private_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = NodeRuntime::start_configured(
            RuntimeConfig {
                base_dir: directory.path(),
                account_id: "bot",
                display_name: "Bot",
                password: "test",
                discovery_port: 0,
                post_office_enabled: false,
                candidate_source: None,
            },
            RuntimeEvents {
                on_dm: Box::new(|_| {}),
                on_channel: Box::new(|_| {}),
                on_file: Box::new(|_| {}),
                on_profile: Box::new(|_| {}),
                on_call_signal: Box::new(|_| {}),
            },
        )
        .await
        .unwrap();
        let host = Arc::new(HeadlessHost::new(runtime.handle()));
        let registry = GrantRegistry::open(
            &directory.path().join("grants.log"),
            "test",
            runtime.account_id(),
        )
        .unwrap();
        let admin_token = new_token();
        let service = Arc::new(IntegrationService::new(
            host.clone(),
            registry,
            &admin_token,
        ));
        let runtime_directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                runtime_directory.path(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let server = LocalServer::bind(runtime_directory.path()).await.unwrap();
        let config = directory.path().join("admin.json");
        write_private_json(
            &config,
            &ClientConfig {
                endpoint: server.endpoint(),
                token: admin_token,
            },
        )
        .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(service, async {
            let _ = stopped.await;
        }));
        let out = directory.path().join("producer.json");
        run(Command::GrantCreate {
            config: config.clone(),
            client_id: "alerts".into(),
            destination: vec![format!("account:{}", "a".repeat(32))],
            expires_in_seconds: 3600,
            describe: false,
            out: out.clone(),
        })
        .await
        .unwrap();
        let producer = LocalClient::from_file(&out).unwrap();
        assert_eq!(
            producer.call("context", json!({})).await.unwrap()["client_id"],
            "alerts"
        );
        assert_eq!(
            producer
                .call("admin_context", json!({}))
                .await
                .unwrap_err()
                .code,
            "SCOPE_DENIED"
        );
        let grants = call(&config, "grant_list", json!({"offset":0,"limit":20}))
            .await
            .unwrap();
        let id = grants["grants"][0]["id"].as_str().unwrap();
        run(Command::GrantRevoke {
            config,
            grant_id: id.into(),
        })
        .await
        .unwrap();
        assert!(producer.call("context", json!({})).await.is_err());
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        host.retire().await;
        runtime.stop().await;
    }
    #[test]
    fn headless_root_refuses_existing_non_bot_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let desktop = directory.path().join("desktop");
        private_directory(&desktop).unwrap();
        std::fs::create_dir(desktop.join("accounts")).unwrap();
        assert!(reserve_bot_root(&desktop).is_err());
        let bot = directory.path().join("bots");
        reserve_bot_root(&bot).unwrap();
        reserve_bot_root(&bot).unwrap();
    }
}
