# File Transfer with Resumable Uploads

## Summary
Add first-class file-sharing to Mesh-Talk chats with resumable transfers. Users can attach files in the chat UI, and the backend coordinates chunked delivery over existing peer connections. When a transfer is interrupted, either side can resume without re-sending completed chunks.

## Goals
- Allow users to send files alongside text conversations.
- Provide progress feedback and basic queuing per conversation.
- Support resumable transfers (pause/resume and automatic recovery after disconnect).
- Persist metadata so partially received files survive app restarts.
- Reuse existing authentication/connection infrastructure without weakening security.

## Non-Goals
- End-to-end encryption of file content beyond current transport guarantees (future work).
- Group file sharing (multi-peer) – scope limited to direct conversations.
- Thumbnail generation or rich file viewers (baseline download/open only).

## Architecture Overview
```
+----------------+        +-----------------+        +------------------+
|  Vue Frontend  | <----> |  Tauri Commands | <----> |   NodeService    |
| (ChatView, UI) |        | (send_file...)  |        | + FileTransferMgr|
+----------------+        +-----------------+        +------------------+
                                                  ^            |
                                                  |            v
                                             +-----------+  +------+
                                             | Storage   |  | TCP  |
                                             | (chunks)  |  | Conn |
                                             +-----------+  +------+
```

### Protocol Additions
Extend `domain::message::Message` with file-specific variants:
- `FileOffer` – announces a new transfer, includes metadata (name, size, mime, checksum, chunk size, transfer_id).
- `FileChunk` – carries chunk data (base64), `transfer_id`, `offset`, `seq_no`.
- `FileAck` – acknowledgement with highest contiguous byte received, optional requested offset to resume.
- `FileComplete` – indicates receiver validated checksum.

Chunk payloads travel over the existing TCP multiplexed channel (JSON per line). Typical chunk size: 64 KiB base64 (approx 48 KiB raw) to balance performance and JSON overhead.

### Backend Components
1. **FileTransferManager** (new module under `src-tauri/src/services/file_transfer.rs`):
   - Tracks outgoing/incoming transfers via `HashMap<TransferId, TransferState>`.
   - Persists state via `storage::file_manager` in `users/<user_id>/transfers/<transfer_id>.manifest`.
   - Provides async APIs: `start_outgoing`, `handle_offer`, `handle_chunk`, `resume_outgoing`, `resume_incoming`, `cancel_transfer`.
   - Exposes event hooks to emit Tauri events (`file-transfer-progress`, `file-transfer-complete`, `file-transfer-error`).

2. **MessageService** updates:
   - Save file messages as `content_type = File` with metadata.
   - Link `chat_messages` table entries to transfer manifests.

3. **NodeService** wiring:
   - Route new `Message::File*` variants to the `FileTransferManager`.
   - Maintain user_id → connection mapping for chunk sending just like chat messages.

4. **Commands** (new Tauri commands):
   - `send_file(path, target_user_id/address)` → returns `transfer_id` and initial message ID.
   - `resume_file(transfer_id)` → resumes pending transfer.
   - `cancel_file(transfer_id)`.
   - `list_file_transfers(conversation_key)` for UI bootstrapping.

5. **Storage Layout**
   - Outgoing chunk cache: `users/<self_id>/transfers/<transfer_id>/chunks/<seq>.bin` (optional streaming direct from source path).
   - Incoming partial file in temp file `*.part` with random-suffix, final rename on completion.
   - Manifest JSON stores offsets, checksums (SHA256), source/dest metadata, status.

### Frontend Flow
1. **UI Updates**
   - `MessageInput.vue`: add attach button; opens OS picker (via Tauri `dialog::open`).
   - `ChatWindow.vue`: render file bubbles with name, size, progress bar, action buttons (pause/resume, retry, open).
   - `appStore`: new actions `sendFile`, `resumeTransfer`, `cancelTransfer`; subscribe to `file-transfer-*` events.
   - `messages` array stores both text and file entries (discriminated by `type`).

2. **State Management**
   - Extend `normalizeMessage` to map backend file messages to UI consumable structure.
   - Maintain `transfers` Map keyed by `transferId` storing progress (bytesSent/Total, status).
   - Auto-resume background: when network reconnects, call `API.files.resumePending()`.

3. **UX Behaviour**
   - When sender picks file, API returns immediate message inserted with status `uploading`.
   - Progress updates via Tauri events update UI in real-time.
   - On completion, message status flips to `sent`/`downloaded`. Provide button to open file location.
   - Receiver sees incoming file message pending; auto-download controlled by settings (future toggle). For baseline, start download automatically but allow pause.

### API Contract (Frontend ↔️ Backend)
```ts
// frontend/src/services/api/files.ts
export const filesAPI = {
  sendFile: (path, target) => invoke("send_file", { path, ...target }),
  resumeFile: (transferId) => invoke("resume_file", { transferId }),
  cancelFile: (transferId) => invoke("cancel_file", { transferId }),
  listTransfers: (conversationKey) => invoke("list_file_transfers", { conversationKey })
};
```
Tauri events:
- `file-transfer-progress` `{ transferId, messageId, bytesSent, bytesTotal, direction }`
- `file-transfer-status` `{ transferId, status: "pending" | "in_progress" | "paused" | "completed" | "failed", error? }`
- `file-transfer-complete` `{ transferId, filePath, checksumValid }`

### Resumable Logic
- Sender monitors acks. If no ack within timeout, pause and wait for connection.
- Manifest for outgoing transfers tracks `lastAckedOffset`. On resume, manager reads manifest, re-opens file, seeks to offset, continues streaming.
- Receiver writes chunk to temp file, updates manifest (highest contiguous offset). When resumed, sends `FileAck` with next required offset.
- After checksum validation, receiver renames `.part` to final name and sends `FileComplete`.
- Both sides clean manifests once `FileComplete` acknowledged.

### Security & Validation
- Verify target conversation is authenticated.
- Limit maximum file size (configurable, e.g., 500 MB) to mitigate abuse.
- Use SHA-256 checksum to detect corruption.
- Sanitize filenames and store within per-user directory to prevent path traversal.

### Error Handling
- If storage write fails, emit `file-transfer-status` with `failed` and reason; keep manifest for retry.
- On network disconnect, transfers move to `paused` state automatically.
- On resume attempts exceeding retry threshold, surface error to UI with manual resume option.

### Testing Strategy
- Unit tests for `FileTransferManager` chunk sequencing, manifest persistence, resume flow.
- Integration test in `src-tauri/tests/file_transfer_test.rs` using in-memory temp directories and simulated TCP peers.
- Frontend vitest cases for store event handling and UI rendering (mock Tauri events).
- Manual QA checklist: large files, network drop simulation, simultaneous transfers, cancellation.

### Milestones
1. **Backend groundwork**: Introduce message variants, FileTransferManager, storage manifests, command scaffolding.
2. **Frontend scaffolding**: API wrapper, store state, UI components without final polish.
3. **Resumable support**: manifests + resume command, ack messaging.
4. **Polish & QA**: progress UI details, error toasts, settings toggles.

### Follow-Up Enhancements
- Optional encryption/compression pipeline.
- Bandwidth throttling per transfer.
- Inline previews for common formats.
- Background transfer dashboard outside chat view.
