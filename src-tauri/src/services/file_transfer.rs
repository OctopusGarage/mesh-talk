use crate::domain::message::{
    FileAckPayload, FileChunkPayload, FileCompletePayload, FileOfferPayload, Message,
};
use crate::events::{
    emit_file_transfer_complete, emit_file_transfer_offer, emit_file_transfer_progress,
    emit_file_transfer_status, FileTransferOfferEvent,
};
use crate::services::node_service::NodeService;
use crate::state::{AppState, SessionInfo};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, RwLock};
use tokio::time::timeout;
use tracing::{error, warn};

const DEFAULT_CHUNK_SIZE: usize = 64 * 1024; // 64 KiB chunks
#[cfg(not(test))]
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
const ACK_TIMEOUT: Duration = Duration::from_millis(50);
const CHUNK_ACK_MAX_RETRIES: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransferDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransferStatus {
    Pending,
    InProgress,
    Paused,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferManifest {
    pub transfer_id: String,
    pub direction: TransferDirection,
    pub file_name: String,
    pub local_path: String,
    pub file_size: u64,
    pub chunk_size: u64,
    pub checksum: Option<String>,
    pub bytes_confirmed: u64,
    pub target_user_id: Option<String>,
    pub target_address: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub status: TransferStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_path: Option<String>,
}

impl TransferManifest {
    fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs();
    }
}

struct PendingAck {
    expected_offset: u64,
    notifier: Arc<Notify>,
}

struct TransferRuntime {
    manifest: TransferManifest,
    pending_ack: Option<PendingAck>,
    error: Option<String>,
}

impl TransferRuntime {
    fn new(manifest: TransferManifest) -> Self {
        Self {
            manifest,
            pending_ack: None,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct FileTransferManager {
    base_dir: PathBuf,
    app_state: AppState,
    node_service: Arc<Mutex<Option<Weak<Mutex<NodeService>>>>>,
    outgoing: Arc<RwLock<HashMap<String, Arc<Mutex<TransferRuntime>>>>>,
    incoming: Arc<RwLock<HashMap<String, Arc<Mutex<TransferRuntime>>>>>,
}

static GLOBAL_MANAGER: OnceLock<Arc<FileTransferManager>> = OnceLock::new();

#[derive(Debug)]
pub enum FileTransferError {
    NoSession,
    MissingTarget,
    InvalidPath,
    Io(std::io::Error),
    Serialization(serde_json::Error),
    AckTimeout,
    TransferNotFound,
    ServiceUnavailable,
    TransferFailed(String),
}

impl fmt::Display for FileTransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileTransferError::NoSession => write!(f, "No active session"),
            FileTransferError::MissingTarget => write!(f, "Target not specified"),
            FileTransferError::InvalidPath => write!(f, "Invalid file path"),
            FileTransferError::Io(err) => write!(f, "I/O error: {err}"),
            FileTransferError::Serialization(err) => {
                write!(f, "Serialization error: {err}")
            }
            FileTransferError::AckTimeout => {
                write!(f, "Timeout waiting for acknowledgement")
            }
            FileTransferError::TransferNotFound => write!(f, "Transfer not found"),
            FileTransferError::ServiceUnavailable => write!(f, "Service not available"),
            FileTransferError::TransferFailed(reason) => {
                write!(f, "Transfer failed: {reason}")
            }
        }
    }
}

impl std::error::Error for FileTransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileTransferError::Io(err) => Some(err),
            FileTransferError::Serialization(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for FileTransferError {
    fn from(value: std::io::Error) -> Self {
        FileTransferError::Io(value)
    }
}

impl From<serde_json::Error> for FileTransferError {
    fn from(value: serde_json::Error) -> Self {
        FileTransferError::Serialization(value)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferHandle {
    pub transfer_id: String,
    pub message: crate::commands::ChatMessageInfo,
}

impl FileTransferManager {
    pub fn init_global<P: AsRef<Path>>(base_dir: P, app_state: AppState) -> Arc<Self> {
        GLOBAL_MANAGER
            .get_or_init(|| {
                Arc::new(Self {
                    base_dir: base_dir.as_ref().to_path_buf(),
                    app_state,
                    node_service: Arc::new(Mutex::new(None)),
                    outgoing: Arc::new(RwLock::new(HashMap::new())),
                    incoming: Arc::new(RwLock::new(HashMap::new())),
                })
            })
            .clone()
    }

    pub fn global() -> Arc<Self> {
        GLOBAL_MANAGER
            .get()
            .expect("FileTransferManager not initialised")
            .clone()
    }

    pub async fn set_node_service(&self, node_service: Arc<Mutex<NodeService>>) {
        let mut guard = self.node_service.lock().await;
        *guard = Some(Arc::downgrade(&node_service));
    }

    fn require_session(&self) -> Result<SessionInfo, FileTransferError> {
        self.app_state
            .session()
            .get()
            .ok_or(FileTransferError::NoSession)
    }

    fn transfers_dir(&self, session: &SessionInfo) -> PathBuf {
        let base = PathBuf::from(&self.base_dir);
        base.join("users")
            .join(&session.user.user_id)
            .join("transfers")
    }

    fn manifests_dir(&self, session: &SessionInfo, direction: TransferDirection) -> PathBuf {
        let mut path = self.transfers_dir(session);
        match direction {
            TransferDirection::Outgoing => path.push("outgoing"),
            TransferDirection::Incoming => path.push("incoming"),
        }
        path
    }

    fn manifest_path(
        &self,
        session: &SessionInfo,
        transfer_id: &str,
        direction: TransferDirection,
    ) -> PathBuf {
        let mut dir = self.manifests_dir(session, direction.clone());
        dir.push(format!("{transfer_id}.manifest"));
        dir
    }

    async fn persist_manifest(
        &self,
        session: &SessionInfo,
        manifest: &TransferManifest,
    ) -> Result<(), FileTransferError> {
        let path = self.manifest_path(session, &manifest.transfer_id, manifest.direction.clone());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_vec_pretty(manifest)?;
        fs::write(path, json).await?;
        Ok(())
    }

    async fn load_manifest(
        &self,
        session: &SessionInfo,
        transfer_id: &str,
        direction: TransferDirection,
    ) -> Result<Option<TransferManifest>, FileTransferError> {
        let path = self.manifest_path(session, transfer_id, direction);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path).await?;
        let manifest = serde_json::from_slice(&bytes)?;
        Ok(Some(manifest))
    }

    async fn load_all_manifests_for_direction(
        &self,
        session: &SessionInfo,
        direction: TransferDirection,
    ) -> Result<Vec<TransferManifest>, FileTransferError> {
        let mut results = Vec::new();
        let dir = self.manifests_dir(session, direction.clone());
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => return Ok(results),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let bytes = fs::read(&path).await?;
                if let Ok(manifest) = serde_json::from_slice::<TransferManifest>(&bytes) {
                    results.push(manifest);
                }
            }
        }
        Ok(results)
    }

    pub async fn start_outgoing(
        &self,
        path: PathBuf,
        target_user_id: Option<String>,
        target_address: Option<String>,
    ) -> Result<FileTransferHandle, FileTransferError> {
        let session = self.require_session()?;

        if !path.exists() || !path.is_file() {
            return Err(FileTransferError::InvalidPath);
        }

        if target_user_id.is_none() && target_address.is_none() {
            return Err(FileTransferError::MissingTarget);
        }

        let metadata = fs::metadata(&path).await?;
        let file_size = metadata.len();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        let checksum = compute_file_checksum(&path).await?;
        let transfer_id = uuid::Uuid::new_v4().to_string();

        let mut manifest = TransferManifest {
            transfer_id: transfer_id.clone(),
            direction: TransferDirection::Outgoing,
            file_name: file_name.clone(),
            local_path: path.to_string_lossy().to_string(),
            file_size,
            chunk_size: DEFAULT_CHUNK_SIZE as u64,
            checksum: Some(checksum.clone()),
            bytes_confirmed: 0,
            target_user_id: target_user_id.clone(),
            target_address: target_address.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|_| Duration::from_secs(0))
                .as_secs(),
            updated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|_| Duration::from_secs(0))
                .as_secs(),
            status: TransferStatus::Pending,
            temp_path: None,
        };

        self.persist_manifest(&session, &manifest).await?;

        let mut content_payload = serde_json::json!({
            "type": "file",
            "transferId": transfer_id,
            "fileName": file_name,
            "fileSize": file_size,
            "checksum": checksum,
            "status": "pending",
            "direction": "outgoing",
            "bytesTransferred": 0
        });

        if let Some(user_id) = target_user_id.clone() {
            content_payload["targetUserId"] = serde_json::Value::String(user_id);
        }
        if let Some(addr) = target_address.clone() {
            content_payload["targetAddress"] = serde_json::Value::String(addr);
        }

        let message_service = self.app_state.message_service();
        let chat_message = message_service
            .create_message(
                session.user.name.clone(),
                session.user.user_id.clone(),
                session.user.address.clone(),
                target_user_id.clone(),
                target_address.clone(),
                content_payload.to_string(),
            )
            .map_err(|err| {
                FileTransferError::TransferFailed(format!("Failed to record message: {err:?}"))
            })?;

        let runtime = Arc::new(Mutex::new(TransferRuntime::new(manifest.clone())));
        {
            let mut guard = self.outgoing.write().await;
            guard.insert(transfer_id.clone(), runtime.clone());
        }

        let manager = self.clone();
        let transfer_id_clone = transfer_id.clone();
        tokio::spawn(async move {
            if let Err(err) = manager
                .run_outgoing_transfer(
                    session.clone(),
                    runtime.clone(),
                    path,
                    target_user_id,
                    target_address,
                )
                .await
            {
                error!("[FileTransfer] Outgoing transfer failed: {err:?}");
                {
                    let mut state = runtime.lock().await;
                    state.manifest.status = TransferStatus::Failed;
                    state.manifest.touch();
                }
                if let Err(persist_err) = manager
                    .persist_manifest(&session, &runtime.lock().await.manifest)
                    .await
                {
                    error!(
                        "[FileTransfer] Failed to persist failed manifest {}: {:?}",
                        transfer_id_clone, persist_err
                    );
                }
                emit_file_transfer_status(
                    &transfer_id_clone,
                    TransferStatus::Failed,
                    Some(format!("{err}")),
                    TransferDirection::Outgoing,
                );
            }
        });

        Ok(FileTransferHandle {
            transfer_id,
            message: crate::commands::ChatMessageInfo::from(chat_message),
        })
    }

    async fn run_outgoing_transfer(
        &self,
        session: SessionInfo,
        runtime: Arc<Mutex<TransferRuntime>>,
        path: PathBuf,
        target_user_id: Option<String>,
        target_address: Option<String>,
    ) -> Result<(), FileTransferError> {
        let offer_payload;
        let manifest_snapshot;
        {
            let state = runtime.lock().await;
            manifest_snapshot = state.manifest.clone();
            offer_payload = FileOfferPayload {
                transfer_id: manifest_snapshot.transfer_id.clone(),
                file_name: manifest_snapshot.file_name.clone(),
                file_size: manifest_snapshot.file_size,
                mime_type: None,
                chunk_size: manifest_snapshot.chunk_size,
                checksum: manifest_snapshot.checksum.clone(),
                resume_offset: Some(manifest_snapshot.bytes_confirmed),
                sender_user_id: Some(session.user.user_id.clone()),
                sender_address: Some(session.user.address.clone()),
                sender_name: Some(session.user.name.clone()),
                request_save_path: Some(false),
            };
        }

        let transfer_id = manifest_snapshot.transfer_id.clone();
        emit_file_transfer_offer(FileTransferOfferEvent {
            transfer_id: transfer_id.clone(),
            file_name: manifest_snapshot.file_name.clone(),
            file_size: manifest_snapshot.file_size,
            checksum: manifest_snapshot.checksum.clone(),
            sender_user_id: Some(session.user.user_id.clone()),
            sender_address: Some(session.user.address.clone()),
            sender_name: Some(session.user.name.clone()),
            request_save_path: Some(false),
        });

        if let Err(err) = FileTransferManager::transmit_with_ack(
            runtime.clone(),
            &manifest_snapshot.transfer_id,
            manifest_snapshot.bytes_confirmed,
            {
                let manager = self.clone();
                let message = Message::FileOffer(offer_payload.clone());
                let target_user_id = target_user_id.clone();
                let target_address = target_address.clone();
                let transfer_id = manifest_snapshot.transfer_id.clone();
                move || {
                    let manager = manager.clone();
                    let message = message.clone();
                    let target_user_id = target_user_id.clone();
                    let target_address = target_address.clone();
                    let transfer_id = transfer_id.clone();
                    Box::pin(async move {
                        manager
                            .send_message(
                                &transfer_id,
                                target_user_id.clone(),
                                target_address.clone(),
                                message.clone(),
                            )
                            .await
                    })
                }
            },
            None,
        )
        .await
        {
            warn!(
                "[FileTransfer] Timed out waiting for initial offer acknowledgement ({}): {:?}",
                transfer_id, err
            );
        }

        let mut offset = {
            let state = runtime.lock().await;
            state.manifest.bytes_confirmed
        };

        let mut file = fs::File::open(&path).await?;
        file.seek(SeekFrom::Start(offset)).await?;

        loop {
            if offset >= manifest_snapshot.file_size {
                break;
            }

            let mut buffer = vec![0u8; DEFAULT_CHUNK_SIZE];
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            buffer.truncate(read);
            let data = general_purpose::STANDARD.encode(&buffer);
            let final_chunk = offset + read as u64 >= manifest_snapshot.file_size;
            let payload = FileChunkPayload {
                transfer_id: manifest_snapshot.transfer_id.clone(),
                offset,
                data,
                final_chunk,
            };

            {
                let mut state = runtime.lock().await;
                state.manifest.status = TransferStatus::InProgress;
                state.manifest.touch();
            }

            FileTransferManager::transmit_with_ack(
                runtime.clone(),
                &manifest_snapshot.transfer_id,
                offset + read as u64,
                {
                    let manager = self.clone();
                    let message = Message::FileChunk(payload.clone());
                    let target_user_id = target_user_id.clone();
                    let target_address = target_address.clone();
                    let transfer_id = manifest_snapshot.transfer_id.clone();
                    move || {
                        let manager = manager.clone();
                        let message = message.clone();
                        let target_user_id = target_user_id.clone();
                        let target_address = target_address.clone();
                        let transfer_id = transfer_id.clone();
                        Box::pin(async move {
                            manager
                                .send_message(
                                    &transfer_id,
                                    target_user_id.clone(),
                                    target_address.clone(),
                                    message.clone(),
                                )
                                .await
                        })
                    }
                },
                Some(CHUNK_ACK_MAX_RETRIES),
            )
            .await?;

            offset = {
                let state = runtime.lock().await;
                state.manifest.bytes_confirmed
            };

            emit_file_transfer_status(
                &manifest_snapshot.transfer_id,
                TransferStatus::InProgress,
                None,
                TransferDirection::Outgoing,
            );

            emit_file_transfer_progress(
                &manifest_snapshot.transfer_id,
                offset,
                manifest_snapshot.file_size,
                TransferDirection::Outgoing,
            );

            self.persist_manifest(&session, &runtime.lock().await.manifest)
                .await?;
        }

        self.send_message(
            &manifest_snapshot.transfer_id,
            target_user_id,
            target_address,
            Message::FileComplete(FileCompletePayload {
                transfer_id: manifest_snapshot.transfer_id.clone(),
                checksum_valid: Some(true),
            }),
        )
        .await?;

        {
            let mut state = runtime.lock().await;
            state.manifest.status = TransferStatus::Completed;
            state.manifest.touch();
        }

        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;
        emit_file_transfer_status(
            &manifest_snapshot.transfer_id,
            TransferStatus::Completed,
            None,
            TransferDirection::Outgoing,
        );
        emit_file_transfer_complete(
            &manifest_snapshot.transfer_id,
            runtime.lock().await.manifest.local_path.clone(),
            TransferDirection::Outgoing,
            true,
        );
        Ok(())
    }

    async fn arm_pending_ack(runtime: Arc<Mutex<TransferRuntime>>, expected_offset: u64) {
        let notifier = Arc::new(Notify::new());
        let mut state = runtime.lock().await;
        state.pending_ack = Some(PendingAck {
            expected_offset,
            notifier,
        });
    }

    async fn transmit_with_ack<F>(
        runtime: Arc<Mutex<TransferRuntime>>,
        transfer_id: &str,
        expected_offset: u64,
        mut send: F,
        max_retries: Option<usize>,
    ) -> Result<(), FileTransferError>
    where
        F: FnMut() -> Pin<Box<dyn Future<Output = Result<(), FileTransferError>> + Send>>,
    {
        let mut attempts = 0usize;

        loop {
            FileTransferManager::arm_pending_ack(runtime.clone(), expected_offset).await;

            if attempts > 0 {
                warn!(
                    "[FileTransfer] Resending transfer {} payload (attempt #{})",
                    transfer_id,
                    attempts + 1
                );
            }

            send().await?;

            match FileTransferManager::wait_for_ack(runtime.clone(), transfer_id).await {
                Ok(()) => return Ok(()),
                Err(FileTransferError::AckTimeout) => {
                    attempts += 1;
                    if let Some(max) = max_retries {
                        if attempts >= max {
                            warn!(
                                "[FileTransfer] Transfer {} exceeded ack retries (max {})",
                                transfer_id, max
                            );
                            return Err(FileTransferError::AckTimeout);
                        }
                    }
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn wait_for_ack(
        runtime: Arc<Mutex<TransferRuntime>>,
        transfer_id: &str,
    ) -> Result<(), FileTransferError> {
        let notify = {
            let mut state = runtime.lock().await;
            if let Some(pending) = state.pending_ack.take() {
                state.pending_ack = Some(PendingAck {
                    expected_offset: pending.expected_offset,
                    notifier: pending.notifier.clone(),
                });
                pending.notifier
            } else {
                return Ok(()); // no ack expected
            }
        };

        timeout(ACK_TIMEOUT, notify.notified())
            .await
            .map_err(|_| FileTransferError::AckTimeout)?;
        // ack handler updates manifest
        Ok(())
    }

    async fn send_message(
        &self,
        transfer_id: &str,
        target_user_id: Option<String>,
        target_address: Option<String>,
        message: Message,
    ) -> Result<(), FileTransferError> {
        let json = serde_json::to_string(&message)?;
        let node_service = {
            let guard = self.node_service.lock().await;
            guard
                .as_ref()
                .and_then(|weak| weak.upgrade())
                .ok_or(FileTransferError::ServiceUnavailable)?
        };

        if let Some(user_id) = target_user_id {
            node_service
                .lock()
                .await
                .send_json_message_to_user(&user_id, json)
                .await
                .map_err(|err| {
                    FileTransferError::TransferFailed(format!(
                        "Failed to send transfer {transfer_id} chunk: {err}"
                    ))
                })?
        } else if let Some(address) = target_address {
            let addr: std::net::SocketAddr = address.parse().map_err(|err| {
                FileTransferError::TransferFailed(format!(
                    "Invalid target address {address}: {err}"
                ))
            })?;
            node_service
                .lock()
                .await
                .send_json_message_to_peer(addr, json)
                .await
                .map_err(|err| {
                    FileTransferError::TransferFailed(format!(
                        "Failed to send transfer {transfer_id} chunk: {err}"
                    ))
                })?
        } else {
            return Err(FileTransferError::MissingTarget);
        }
        Ok(())
    }

    async fn get_or_create_incoming_runtime(
        &self,
        session: &SessionInfo,
        transfer_id: &str,
    ) -> Result<Arc<Mutex<TransferRuntime>>, FileTransferError> {
        if let Some(existing) = {
            let guard = self.incoming.read().await;
            guard.get(transfer_id).cloned()
        } {
            return Ok(existing);
        }

        let manifest = self
            .load_manifest(session, transfer_id, TransferDirection::Incoming)
            .await?
            .ok_or(FileTransferError::TransferNotFound)?;
        let runtime = Arc::new(Mutex::new(TransferRuntime::new(manifest)));
        let mut guard = self.incoming.write().await;
        guard.insert(transfer_id.to_string(), runtime.clone());
        Ok(runtime)
    }

    async fn accept_incoming_transfer_internal(
        &self,
        session: SessionInfo,
        runtime: Arc<Mutex<TransferRuntime>>,
        transfer_id: String,
        explicit_path: Option<String>,
    ) -> Result<(), FileTransferError> {
        let snapshot = {
            let state = runtime.lock().await;
            state.manifest.clone()
        };

        let file_name = snapshot.file_name.clone();
        let sender_user_id = snapshot.target_user_id.clone();
        let sender_address = snapshot.target_address.clone();
        let bytes_confirmed = snapshot.bytes_confirmed;
        let file_size = snapshot.file_size;

        let desired_final_path = explicit_path
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                if !snapshot.local_path.is_empty() {
                    Some(PathBuf::from(snapshot.local_path.clone()))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                self.transfers_dir(&session)
                    .join("downloads")
                    .join(file_name.clone())
            });

        if let Some(parent) = desired_final_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).await?;
            }
        }

        let temp_path = snapshot.temp_path.map(PathBuf::from).unwrap_or_else(|| {
            let mut temp = desired_final_path.clone();
            let new_ext = match desired_final_path.extension() {
                Some(ext) => format!("{}.part", ext.to_string_lossy()),
                None => "part".to_string(),
            };
            temp.set_extension(new_ext);
            temp
        });

        {
            let mut state = runtime.lock().await;
            state.manifest.local_path = desired_final_path.to_string_lossy().to_string();
            state.manifest.temp_path = Some(temp_path.to_string_lossy().to_string());
            state.manifest.status = TransferStatus::InProgress;
            state.manifest.touch();
        }

        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;

        emit_file_transfer_status(
            &transfer_id,
            TransferStatus::InProgress,
            None,
            TransferDirection::Incoming,
        );
        emit_file_transfer_progress(
            &transfer_id,
            bytes_confirmed,
            file_size,
            TransferDirection::Incoming,
        );

        self.send_message(
            &transfer_id,
            sender_user_id,
            sender_address,
            Message::FileAck(FileAckPayload {
                transfer_id: transfer_id.clone(),
                received_offset: bytes_confirmed,
                request_offset: Some(bytes_confirmed),
            }),
        )
        .await?;

        let temp_file_path = {
            let state = runtime.lock().await;
            state.manifest.temp_path.clone()
        };
        if let Some(temp_path) = temp_file_path {
            let _ = OpenOptions::new()
                .create(true)
                .write(true)
                .open(Path::new(&temp_path))
                .await;
        }

        Ok(())
    }

    pub async fn handle_file_offer(
        &self,
        mut payload: FileOfferPayload,
        fallback_address: Option<String>,
    ) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let sender_user_id = payload.sender_user_id.clone();
        if payload.sender_address.is_none() {
            payload.sender_address = fallback_address;
        }
        let sender_address = payload.sender_address.clone();

        let downloads_dir = self.transfers_dir(&session).join("downloads");
        let default_final_path = downloads_dir.join(&payload.file_name);

        let existing_manifest = self
            .load_manifest(&session, &payload.transfer_id, TransferDirection::Incoming)
            .await?;
        let mut manifest = existing_manifest.unwrap_or_else(|| TransferManifest {
            transfer_id: payload.transfer_id.clone(),
            direction: TransferDirection::Incoming,
            file_name: payload.file_name.clone(),
            local_path: default_final_path.to_string_lossy().to_string(),
            file_size: payload.file_size,
            chunk_size: payload.chunk_size,
            checksum: payload.checksum.clone(),
            bytes_confirmed: payload.resume_offset.unwrap_or(0),
            target_user_id: sender_user_id.clone(),
            target_address: sender_address.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|_| Duration::from_secs(0))
                .as_secs(),
            updated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|_| Duration::from_secs(0))
                .as_secs(),
            status: TransferStatus::Pending,
            temp_path: None,
        });
        manifest.target_user_id = sender_user_id.clone();
        manifest.target_address = sender_address.clone();
        manifest.touch();

        let runtime = Arc::new(Mutex::new(TransferRuntime::new(manifest.clone())));
        {
            let mut guard = self.incoming.write().await;
            guard.insert(payload.transfer_id.clone(), runtime.clone());
        }

        let require_prompt =
            manifest.temp_path.is_none() && payload.request_save_path.unwrap_or(true);

        self.persist_manifest(&session, &manifest).await?;

        if require_prompt {
            emit_file_transfer_status(
                &payload.transfer_id,
                TransferStatus::Pending,
                None,
                TransferDirection::Incoming,
            );
            emit_file_transfer_offer(FileTransferOfferEvent {
                transfer_id: payload.transfer_id.clone(),
                file_name: payload.file_name.clone(),
                file_size: payload.file_size,
                checksum: payload.checksum.clone(),
                sender_user_id,
                sender_address,
                sender_name: payload.sender_name.clone(),
                request_save_path: Some(true),
            });
            return Ok(());
        }

        self.accept_incoming_transfer_internal(session, runtime, payload.transfer_id.clone(), None)
            .await
    }

    pub async fn accept_incoming_transfer(
        &self,
        transfer_id: &str,
        save_path: Option<String>,
    ) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let runtime = self
            .get_or_create_incoming_runtime(&session, transfer_id)
            .await?;
        self.accept_incoming_transfer_internal(session, runtime, transfer_id.to_string(), save_path)
            .await
    }

    pub async fn reject_incoming_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let runtime = self
            .get_or_create_incoming_runtime(&session, transfer_id)
            .await?;

        let (sender_user_id, sender_address) = {
            let state = runtime.lock().await;
            (
                state.manifest.target_user_id.clone(),
                state.manifest.target_address.clone(),
            )
        };

        {
            let mut state = runtime.lock().await;
            state.manifest.status = TransferStatus::Failed;
            state.manifest.touch();
        }
        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;

        emit_file_transfer_status(
            transfer_id,
            TransferStatus::Failed,
            Some("Receiver cancelled".into()),
            TransferDirection::Incoming,
        );

        self.send_message(
            transfer_id,
            sender_user_id,
            sender_address,
            Message::FileComplete(FileCompletePayload {
                transfer_id: transfer_id.to_string(),
                checksum_valid: Some(false),
            }),
        )
        .await?;

        {
            let mut guard = self.incoming.write().await;
            guard.remove(transfer_id);
        }

        Ok(())
    }

    pub async fn handle_file_chunk(
        &self,
        payload: FileChunkPayload,
    ) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let runtime = {
            let guard = self.incoming.read().await;
            guard.get(&payload.transfer_id).cloned()
        }
        .ok_or(FileTransferError::TransferNotFound)?;

        let mut state = runtime.lock().await;
        let expected_offset = state.manifest.bytes_confirmed;
        if payload.offset != expected_offset {
            warn!(
                "[FileTransfer] Unexpected chunk offset for {} expected {} got {}",
                payload.transfer_id, expected_offset, payload.offset
            );
            return Ok(());
        }

        let temp_path = state
            .manifest
            .temp_path
            .clone()
            .ok_or_else(|| FileTransferError::TransferFailed("Missing temp path".into()))?;

        let decoded = general_purpose::STANDARD
            .decode(payload.data.as_bytes())
            .map_err(|err| {
                FileTransferError::TransferFailed(format!("Base64 decode error: {err}"))
            })?;

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&temp_path)
            .await?;
        file.seek(SeekFrom::Start(payload.offset)).await?;
        file.write_all(&decoded).await?;
        file.flush().await?;

        state.manifest.bytes_confirmed = payload.offset + decoded.len() as u64;
        state.manifest.touch();
        let complete = state.manifest.bytes_confirmed >= state.manifest.file_size;
        drop(state);

        let target_user_id = runtime.lock().await.manifest.target_user_id.clone();
        let target_address = runtime.lock().await.manifest.target_address.clone();

        self.send_message(
            &payload.transfer_id,
            target_user_id.clone(),
            target_address.clone(),
            Message::FileAck(FileAckPayload {
                transfer_id: payload.transfer_id.clone(),
                received_offset: runtime.lock().await.manifest.bytes_confirmed,
                request_offset: None,
            }),
        )
        .await?;

        emit_file_transfer_progress(
            &payload.transfer_id,
            runtime.lock().await.manifest.bytes_confirmed,
            runtime.lock().await.manifest.file_size,
            TransferDirection::Incoming,
        );

        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;

        if complete {
            self.finalize_incoming(&session, runtime.clone()).await?;
        }

        Ok(())
    }

    async fn finalize_incoming(
        &self,
        session: &SessionInfo,
        runtime: Arc<Mutex<TransferRuntime>>,
    ) -> Result<(), FileTransferError> {
        let mut state = runtime.lock().await;
        let manifest = &mut state.manifest;
        let temp_path = manifest
            .temp_path
            .clone()
            .ok_or_else(|| FileTransferError::TransferFailed("Missing temp path".into()))?;
        let final_path = manifest.local_path.clone();
        fs::rename(&temp_path, &final_path).await?;
        manifest.status = TransferStatus::Completed;
        manifest.touch();
        drop(state);

        self.persist_manifest(session, &runtime.lock().await.manifest)
            .await?;
        emit_file_transfer_status(
            &runtime.lock().await.manifest.transfer_id,
            TransferStatus::Completed,
            None,
            TransferDirection::Incoming,
        );
        emit_file_transfer_complete(
            &runtime.lock().await.manifest.transfer_id,
            final_path,
            TransferDirection::Incoming,
            true,
        );
        let transfer_id = runtime.lock().await.manifest.transfer_id.clone();
        {
            let mut guard = self.incoming.write().await;
            guard.remove(&transfer_id);
        }
        Ok(())
    }

    pub async fn handle_file_ack(&self, payload: FileAckPayload) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let runtime = {
            let guard = self.outgoing.read().await;
            guard.get(&payload.transfer_id).cloned()
        }
        .ok_or(FileTransferError::TransferNotFound)?;

        {
            let mut state = runtime.lock().await;
            if let Some(pending) = state.pending_ack.take() {
                state.manifest.bytes_confirmed = payload.received_offset;
                state.manifest.touch();
                pending.notifier.notify_waiters();
            }
        }

        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;
        emit_file_transfer_progress(
            &payload.transfer_id,
            runtime.lock().await.manifest.bytes_confirmed,
            runtime.lock().await.manifest.file_size,
            TransferDirection::Outgoing,
        );

        Ok(())
    }

    pub async fn handle_file_complete(
        &self,
        payload: FileCompletePayload,
    ) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let runtime = {
            let guard = self.outgoing.read().await;
            guard.get(&payload.transfer_id).cloned()
        }
        .ok_or(FileTransferError::TransferNotFound)?;

        {
            let mut state = runtime.lock().await;
            state.manifest.status = TransferStatus::Completed;
            state.manifest.touch();
        }
        self.persist_manifest(&session, &runtime.lock().await.manifest)
            .await?;
        emit_file_transfer_status(
            &payload.transfer_id,
            TransferStatus::Completed,
            None,
            TransferDirection::Outgoing,
        );
        emit_file_transfer_complete(
            &payload.transfer_id,
            runtime.lock().await.manifest.local_path.clone(),
            TransferDirection::Outgoing,
            payload.checksum_valid.unwrap_or(true),
        );
        {
            let mut guard = self.outgoing.write().await;
            guard.remove(&payload.transfer_id);
        }
        Ok(())
    }

    pub async fn resume_transfer(&self, transfer_id: &str) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        let manifest = self
            .load_manifest(&session, transfer_id, TransferDirection::Outgoing)
            .await?
            .ok_or(FileTransferError::TransferNotFound)?;
        let path = PathBuf::from(&manifest.local_path);
        if !path.exists() {
            return Err(FileTransferError::InvalidPath);
        }

        let transfer_id_owned = transfer_id.to_string();
        let runtime = Arc::new(Mutex::new(TransferRuntime::new(manifest.clone())));
        {
            let mut guard = self.outgoing.write().await;
            guard.insert(transfer_id_owned.clone(), runtime.clone());
        }

        emit_file_transfer_status(
            &transfer_id_owned,
            TransferStatus::InProgress,
            None,
            TransferDirection::Outgoing,
        );

        let manager = self.clone();
        let transfer_id_for_spawn = transfer_id_owned.clone();
        tokio::spawn(async move {
            if let Err(err) = manager
                .run_outgoing_transfer(
                    session.clone(),
                    runtime,
                    path,
                    manifest.target_user_id.clone(),
                    manifest.target_address.clone(),
                )
                .await
            {
                error!(
                    "[FileTransfer] Resume failed for {}: {:?}",
                    transfer_id_for_spawn, err
                );
                emit_file_transfer_status(
                    &transfer_id_for_spawn,
                    TransferStatus::Failed,
                    Some(format!("{err}")),
                    TransferDirection::Outgoing,
                );
            }
        });

        Ok(())
    }

    pub async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), FileTransferError> {
        let session = self.require_session()?;
        {
            let mut guard = self.outgoing.write().await;
            guard.remove(transfer_id);
        }
        {
            let mut guard = self.incoming.write().await;
            guard.remove(transfer_id);
        }

        let mut manifest = if let Some(manifest) = self
            .load_manifest(&session, transfer_id, TransferDirection::Outgoing)
            .await?
        {
            manifest
        } else if let Some(manifest) = self
            .load_manifest(&session, transfer_id, TransferDirection::Incoming)
            .await?
        {
            manifest
        } else {
            return Err(FileTransferError::TransferNotFound);
        };

        let direction = manifest.direction.clone();
        manifest.status = TransferStatus::Paused;
        manifest.touch();
        self.persist_manifest(&session, &manifest).await?;
        emit_file_transfer_status(transfer_id, TransferStatus::Paused, None, direction);
        Ok(())
    }

    pub async fn list_transfers(&self) -> Result<Vec<TransferManifest>, FileTransferError> {
        let session = self.require_session()?;
        let mut manifests = self
            .load_all_manifests_for_direction(&session, TransferDirection::Outgoing)
            .await?;
        manifests.extend(
            self.load_all_manifests_for_direction(&session, TransferDirection::Incoming)
                .await?,
        );
        Ok(manifests)
    }
}

async fn compute_file_checksum(path: &Path) -> Result<String, FileTransferError> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{sleep, Duration as TokioDuration};

    fn test_manifest(direction: TransferDirection) -> TransferManifest {
        TransferManifest {
            transfer_id: "test-transfer".to_string(),
            direction,
            file_name: "test.bin".to_string(),
            local_path: "test.bin".to_string(),
            file_size: 1024,
            chunk_size: DEFAULT_CHUNK_SIZE as u64,
            checksum: None,
            bytes_confirmed: 0,
            target_user_id: None,
            target_address: None,
            created_at: 0,
            updated_at: 0,
            status: TransferStatus::Pending,
            temp_path: None,
        }
    }

    #[tokio::test]
    async fn wait_for_ack_succeeds_after_notification() {
        let runtime = Arc::new(Mutex::new(TransferRuntime::new(test_manifest(
            TransferDirection::Outgoing,
        ))));

        FileTransferManager::arm_pending_ack(runtime.clone(), 0).await;

        let notifier = {
            let state = runtime.lock().await;
            state.pending_ack.as_ref().unwrap().notifier.clone()
        };

        tokio::spawn(async move {
            sleep(TokioDuration::from_millis(10)).await;
            notifier.notify_waiters();
        });

        FileTransferManager::wait_for_ack(runtime.clone(), "test")
            .await
            .expect("ack should be received");
    }

    #[tokio::test]
    async fn transmit_with_ack_retries_until_notified() {
        let runtime = Arc::new(Mutex::new(TransferRuntime::new(test_manifest(
            TransferDirection::Outgoing,
        ))));

        let attempts = Arc::new(AtomicUsize::new(0));
        let runtime_for_send = runtime.clone();
        let attempts_for_send = attempts.clone();

        FileTransferManager::transmit_with_ack(
            runtime.clone(),
            "test",
            128,
            move || {
                let runtime = runtime_for_send.clone();
                let attempts = attempts_for_send.clone();
                Box::pin(async move {
                    let current = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if current == 3 {
                        let notifier = {
                            let state = runtime.lock().await;
                            state.pending_ack.as_ref().unwrap().notifier.clone()
                        };
                        tokio::spawn(async move {
                            sleep(TokioDuration::from_millis(10)).await;
                            notifier.notify_waiters();
                        });
                    }
                    Ok(())
                })
            },
            Some(5),
        )
        .await
        .expect("ack should arrive before retry limit");

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn transmit_with_ack_fails_after_max_retries() {
        let runtime = Arc::new(Mutex::new(TransferRuntime::new(test_manifest(
            TransferDirection::Outgoing,
        ))));

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_send = attempts.clone();

        let result = FileTransferManager::transmit_with_ack(
            runtime.clone(),
            "test",
            256,
            move || {
                let attempts = attempts_for_send.clone();
                Box::pin(async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
            Some(3),
        )
        .await;

        assert!(matches!(result, Err(FileTransferError::AckTimeout)));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
