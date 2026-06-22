# Chat UI — manual smoke checklist

This is a **supplementary** smoke checklist for the desktop chat UI (the "Ink & Signal"
redesign). It is meant for a quick human walk-through of the real app; it is *not* the
primary regression net:

- The **front-end flow** (login → roster → DM/channel → react/mention/reply → search →
  files → verify → settings → diagnostics → pin → sign out) is covered by an automated
  Playwright suite that drives the UI against a mocked Tauri backend, keyed off
  `data-testid` attributes: `cd frontend && npm run e2e`.
- The **multi-process backend behavior** (real `mesh-talk-node` processes discovering each
  other over UDP and talking over TCP, persistence, post-office offline delivery) is
  covered by the end-to-end rigs: `make e2e`.

Run those first. Use the steps below when you want to eyeball the actual desktop app or
verify something the automated suites can't (real network discovery, native dialogs, look
and feel).

## Setup (two instances on one machine)

Run two app instances with separate `HOME` dirs so their per-account data
(`~/.mesh-talk/accounts/<account>/`) doesn't collide:

```bash
# Terminal 1
HOME=/tmp/mesh-a make tauri-dev    # (or: cd src-tauri && cargo tauri dev)
# Terminal 2
HOME=/tmp/mesh-b make tauri-dev
```

Optionally start a post office in a third `HOME` to exercise offline delivery:

```bash
HOME=/tmp/mesh-po cargo run -p mesh-talk-core --bin mesh-talk-node -- \
    --keystore /tmp/mesh-po/device.keystore --password pw --name relay --post-office
```

> A node does a password-KDF keystore unlock on login, so give it a few seconds to come up
> after you sign in. A post office cold-starts slowly (~15 s on first run). If an action
> errors with "node not started", the node is still opening — wait and retry.

## Walk-through

1. **Register / sign in.** On the LoginScreen, use the **Register** tab to create a
   different account in each instance (e.g. `alice` / `bob`), then sign in. The app lands on
   the chat shell; no blank/black screen.
2. **Roster + presence.** Within a few seconds each instance lists the other in the
   **Sidebar** conversation list, with an identity glyph and a presence dot. A running post
   office is tagged in Diagnostics → Peers.
3. **DM.** Select the peer to open the **ConversationView** (crest header). Type in the
   **Composer** and send: the bubble appears immediately on the sender (optimistic) and
   shortly after on the recipient (live). Reply back the other way.
   - `Enter` sends; `Shift+Enter` inserts a newline. URLs render as clickable links.
4. **Channel.** Create a channel via **CreateChannel**, add the peer via **Members**, and
   exchange messages there too.
5. **React.** Hover a message and add an emoji reaction; the chip appears for both sides.
6. **@-mention.** In a channel, type `@` and pick a member from the autocomplete; the
   mention is highlighted, and the mentioned user sees a "mentioned you" marker.
7. **Reply.** Use the reply action on a message; the Composer shows a reply banner, and the
   sent message carries the parent snippet.
8. **Search.** Open the **Search** dialog, query a known word, click a result, and confirm
   it navigates into the right conversation.
9. **Files.** Attach a file in the Composer; watch transfer progress, and confirm the
   recipient can save it. Open the **Files** tray to review shared files.
10. **Verify contact.** Open **VerifyContact** for a DM peer; confirm the **safety number**
    matches on both instances, then mark the contact verified.
11. **Settings.** Open **Settings**: toggle theme (light / dark / oled), switch language
    (EN ↔ 中文) and confirm labels re-render, and review tray / autostart / notifications /
    download-folder options.
12. **Diagnostics.** Open **Diagnostics**: Overview shows environment + your identity; Peers
    lists the discovered roster with addresses; Logs and Troubleshoot tabs render.
13. **Pin.** Pin a contact; it moves into the **Pinned** section of the Sidebar.
14. **Durability.** Restart one instance, sign in, reopen the conversation → prior messages
    are still there (persisted history).
15. **Sign out.** Sign out → returns to the LoginScreen.

## Expected

- Peers appear by name/identity with presence dots; a post office is visible in Diagnostics.
- Messages, reactions, mentions, replies, and files flow both ways live.
- Safety numbers match between paired instances; verification sticks.
- Theme/language changes apply immediately; pinned and history survive restart.
