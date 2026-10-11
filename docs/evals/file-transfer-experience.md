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

## Follow-up evaluation — 2026-10-10

`make e2e` passed all five real multi-process scenarios, including encrypted file
transfer between two CLI nodes. The deterministic `make eval-smoke` passed, and
`make ai-eval` passed all five model-backed agent-contract cases using Codex CLI
in read-only mode (`target/ai-eval/report.json`).

A separate read-only AI review of this transfer change found three issues in the
interaction code: saved-path cache eviction could leave a stale ready status, a
sender bar could round to 100% before completion, and the sender bar had no
accessible progress semantics. The fixes invalidate evicted readiness, cap active
progress at 99%, and add a named progressbar with measured values and polite
announcements at ten-point intervals. A second AI review confirmed those fixes
but found an in-flight status-reply race; generation invalidation and a delayed
reply regression test now cover it. A final read-only AI check passed that fix
(`target/ai-eval/file-transfer-race-check.json`).

Focused frontend unit tests (5), browser scenarios (2), typecheck, and lint passed
after the fixes. Model reviews inspected source and tests; browser and Rust test
results are the execution evidence. The browser suite still uses mocked Tauri IPC,
while the CLI E2E uses real nodes; a two-desktop transfer on a real network remains
a separate manual check.

The full `./scripts/check-health.sh` gate passed on the final code: 278 frontend
unit tests, 229 browser scenarios passed with one intentional site-capture skip,
713 core and 90 Tauri unit tests, the remaining workspace tests, formatting,
typecheck, lint, dependency and secret scans, security audits, and both builds.

## Directory attachments — 2026-10-10

The folder picker and native drop path now accept directories. The sender packs
regular files and empty subdirectories into a bounded tar payload and sends it through
the existing encrypted file transfer. The manifest retains MFM3 framing and carries a
dedicated directory MIME marker; older MFM3-capable clients can download the `.tar` file using their
existing Save action. New clients show a folder icon and save into the configured or
system Downloads directory by default, extracting only after whole-file verification.
The Received files tray offers Save as… when a different destination is wanted.
Extraction rejects traversal,
links, special entries, too many entries, and expansion beyond the transfer size. It
stages privately and renames the completed folder into place.

Attachment approval is manual by default for files, folders and videos. The recipient
can Receive or Decline an offer in chat or the Received files tray, enable automatic
receive globally, or override the policy for one contact from the conversation header.
Raster images sent as media up to 16 MiB, stickers and text continue automatically;
SVG and larger images require approval. Automatic images must also pass a format
signature check on the first decrypted chunk before durable ingest. A recipient decision
is stored before a positive consent response is returned to a sender. The sender's
progress labels show waiting for approval or a declined offer, and no chunk percentage
advances before consent. Legacy senders can still submit an offer, but a current
recipient refuses unsolicited chunks until approval; old recipients use the existing
direct-transfer path. Channel file/folder offers wait for members to pull after approval.
Approved partial files resume pulling after restart. The sender must be reachable
after an offline recipient approves because approval precedes chunk transfer. The CLI
prints an offer ID and accepts `/receive <file-id>` or `/decline <file-id>`.

Read-only AI reviews (`target/ai-eval/attachment-consent-review.md`,
`attachment-consent-followup.md`, `attachment-consent-closure.md`) found
offer-binding, sender-author, restart-resume, relay-drain, account-switch, and
automatic-image admission gaps. The implementation now binds equivalent offers
to one sender and decision, requeues approved partial files on restart, skips
unapproved relay chunks, refreshes settings on identity change, and rejects
oversized, SVG, and malformed raster offers from the automatic path. The final
targeted review (`target/ai-eval/attachment-consent-final.md`) found the two
remaining duplicate-offer and sequence-2 bypass findings closed. These reviews
were source inspection; the loopback, CLI, browser, and health suites provide
the executable verification.

The read-only model review (`target/ai-eval/directory-transfer-review.md`) found a
retry-loss path: pruning archive chunks before extraction succeeds could make a failed
save permanent. The save path now defers completion and pruning until after extraction,
rename, and directory synchronization. The loopback test models an aborted extraction
after archive verification and retries the folder save. A follow-up review
(`target/ai-eval/directory-transfer-followup.md`) confirmed the retry fix and found
that names legal on Unix could prevent saving on Windows. Extraction now sanitizes
each path component, maps the original directory tree to collision-safe local names,
and tests illegal characters, reserved device names, and collisions. A final read-only
review (`target/ai-eval/directory-transfer-final.md`) caught superscript device
names and Unix backslashes that Windows interprets as separators. The sanitizer
now covers the documented superscript variants; the sender normalizes archive
components and resolves sibling collisions before packaging. The closure review
(`target/ai-eval/directory-transfer-closure.md`) found no remaining high or medium
issue in those paths; executable tests remain the verification evidence.

Evidence: the core pack/extract tests cover nested and empty folders, path rejection,
and link rejection; the two-node TCP test covers real encrypted folder transfer,
extraction, and the legacy tar save path. `directory-transfer.spec.ts` covers folder
picker wiring and recipient readiness gating. The browser test uses mocked Tauri IPC;
the Rust test exercises two real nodes over loopback.

## Sender progress — 2026-10-11

Account file, image, and directory sends now show local preparation separately from
recipient-confirmed chunk transfer. The sender bar stays below 100% while the final
durable custody check is pending. It shows a remaining-time estimate only after two
advancing recipient observations, then clears on the qualified final receipt. An
offline recipient shows a waiting state without a speculative ETA. Channel file
distribution still uses its existing best-effort group flow and only reports local
preparation; it has no per-member durable receipt record to support a truthful
recipient-confirmed percentage.

The read-only model review (`target/ai-eval/sender-transfer-progress-review.md`)
identified unbounded completed-transfer state and avoidable per-round Have-set
memory. The transfer store now caps records at 1,024 and evicts completed rows
first; recipient Have IDs are sorted and deduplicated in place before counting
the intersection with local chunk IDs. The review did not execute tests.
A follow-up read-only review (`target/ai-eval/sender-transfer-progress-followup.md`)
found no remaining high or medium issue in those two fixes.

The two-node core test checks that an early file-card receipt cannot claim chunk
completion and that the final custody receipt completes progress. The sync test
asserts an intermediate recipient Have count. Browser cases cover account files,
inline images, ETA, confirmation, and concurrent sends. The portable UI matrix
runs these cases on Linux, Windows, and macOS in GitHub Actions with browser
evidence; local macOS Chromium and WebKit runs validated all 29 core scenarios.
