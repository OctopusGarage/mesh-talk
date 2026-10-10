# File and image transfer experience review

## Scope

Reviewed file and image selection, optimistic sending, backend staging, manifest arrival,
background chunk sync, inline preview, Received files, conversation history, saving, and
reopening saved downloads. The protocol and encrypted chunk transfer remain unchanged.

## Findings and changes

| Finding | Impact | Resolution |
| --- | --- | --- |
| Recipient background sync exposed no progress to the UI. The existing `file-progress` event covered sender staging and save-to-disk only. | A file card appeared complete while its chunks were still arriving. | A bounded `file_statuses` IPC snapshot reports held/total chunks and authoritative readiness. Visible files share one 750 ms polling loop. |
| Chat, Received files, and conversation history enabled Save as soon as the manifest appeared. | Users could open a save dialog that ended in “file incomplete.” | Save and Save as remain disabled until the node reports that complete bytes or a durable media copy are available. Save handlers check readiness again before starting. |
| Inline media tried to read incomplete bytes and could leave an empty bubble while retrying. | Recipients saw a blank image or video card. | The media card shows the filename and receive progress first; byte loading and preview begin only when ready. |
| Saved attachments could be offered for another download after chunks were pruned. | Repeat saves failed despite a valid local copy. | Chat, tray, and history use the remembered destination and offer Reveal. |
| Account sends ignored their staging progress callback, and conversation-keyed sender events collided when two files were sent together. | Senders could not tell which file was advancing. | Account and channel sends now emit throttled progress keyed to each optimistic file message; each pending bubble shows its own bar. |

## Interaction contract

| State | Chat and history | Received files |
| --- | --- | --- |
| Sender staging | Pending file card with its own progress bar | Not applicable |
| Manifest not yet available | Waiting label; no byte action | Waiting label; Save disabled |
| Chunks arriving | Per-file percentage and progress bar; Save disabled | Same percentage; Save and Save as disabled |
| Complete according to the node | Preview or enabled Save | Enabled Save and Save as |
| Saved locally | Reveal saved location | Reveal saved location |

File card delivery and file byte completion are separate facts. The automatic delivery
receipt therefore keeps its existing meaning. Polling stops for a file once the node
reports it ready, and status is cleared when the runtime identity changes.

## Evidence

- `file-receive-controls.spec.ts`: an attachment held at 50% stays visible and cannot be
  saved from chat, history, or tray; when readiness changes, all controls update, one
  save works, and history switches to Reveal.
- `media-recovery.spec.ts`: an image held at 50% is not read early and becomes previewable
  after bytes arrive, without a conversation remount.
- `file-send-progress.spec.ts`: two simultaneous account sends show independent progress
  in their respective pending file bubbles.
- Existing sender delivery, tray membership, and remembered download browser scenarios
  remain in the focused regression set.
- The core loopback test `two_nodes_transfer_a_file_over_loopback_tcp` exercises real
  encrypted file delivery and receipt.

The browser scenarios use mocked Tauri commands to control intermediate states. They do
not replace a manual check with two desktop nodes on a real network, where transfer speed,
disconnects, and resume timing depend on the environment.

Validation run on this branch: `make test`, `cargo clippy --all-targets -- -D warnings`,
frontend typecheck, lint, unit tests (276 passing), production build, format check, and
the seven focused Playwright scenarios covering send progress, receive controls, media
recovery, tray membership, remembered downloads, and automatic delivery.
