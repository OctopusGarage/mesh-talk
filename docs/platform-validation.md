# Core workflow and platform evaluations

Product evaluations are separate from prompt/agent/model evaluations. A successful
build, coverage percentage, PNG file signature or mocked browser session alone is
not evidence that the shipped desktop workflow works.

## Layers and honest boundaries

The protected CI matrix runs Rust workspace unit/integration tests and frontend
Vitest unit tests on Linux, macOS and Windows. Linux records Rust `llvm-cov` and
frontend V8 coverage; the frontend report includes every production `.ts` and
`.tsx` file, including files with zero unit coverage. The `src/store` line gate
is 70%. Run `cd frontend && npm run test:coverage` locally to inspect the
frontend report in `frontend/coverage/`. Browser and native E2E assertions do
not contribute to this unit coverage percentage, so low component percentages
must be read alongside the scenario matrix below.

| Layer            | Systems exercised                                                                                                                    | Platforms                                                  | Boundary                                                                                                                             |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Backend core     | Real CLI processes, signed discovery, encrypted TCP, DM/channel delivery, process restart, relay delivery, received file bytes       | Linux, Windows, macOS                                      | No renderer; discovery can use its signed unicast fallback, so a passed scenario does not prove multicast-only discovery             |
| Native core      | Actual Tauri builder/webview, embedded dev-only W3C driver, real CLI peer, persisted production stores, native keyboard/dialog input | Linux GTK/WebKit, Windows WebView2, macOS WKWebView/AppKit | No IPC replacement; test example uses existing cheap test KDF, separately built CLI and production application retain production KDF |
| Portable browser | Real React UI and visible interactions with a deterministic mocked IPC/event boundary                                                | Chromium on Linux/Windows/macOS; WebKit on macOS           | Guards UI wiring/layout, not native transport, OS chrome or Rust authentication                                                      |
| Model-backed AI  | External model responses scored against prompt/agent cases                                                                           | Configured provider                                        | Not a product E2E; an unavailable provider means **not run**, never a real-model pass                                                |

Hosted runners cannot establish behavior for every physical camera, microphone,
GPU, monitor scale, compositor or LAN firewall. Those remain explicit manual
acceptance checks; do not convert unavailable devices to passing observations.

## Backend execution and evidence

Run from the repository root:

```sh
EVAL_OUTPUT_DIR=/tmp/mesh-talk-backend-eval node scripts/evals/backend-runner.mjs
```

The runner selects the four integration test targets explicitly and runs ignored
tests serially, continuing through independent suites after a failure while
retaining the overall failing verdict. It requires all five named scenarios exactly once, a matching
Cargo test count and zero failures/ignored/filtered tests. An exit-zero invocation
which selects no tests is a failure. The JSON report binds source revision,
platform, timings and SHA256 log digests. Artifacts contain test observations and
redacted logs, never fixture keystores or account directories.

Normal CLI shutdown sends `/quit` and waits for the owned child to exit before
reusing its store. The test helper never removes a child's lock to manufacture a
successful restart. Forced termination remains a separate recovery boundary,
particularly on Windows where conservative stale-lock handling needs care.

The offline relay test must stop the sender before the receiver returns. A test
with the sender still online cannot independently establish post-office delivery;
ordinary direct synchronization could satisfy the same message assertion. Failure
of this stricter route-isolated scenario must be investigated, not hidden by
relaxing it or removing it from the required matrix.

## Native scenarios and evidence

The native runner retains the 13 contact-hiding/invisible-mode scenarios and adds
registration/sign-in, logout/invalid-password recovery, signed discovery,
bidirectional DM, Unicode/multiline/long content, channel delivery, attachment
rendering, history after real process restart, theme persistence, profile/avatar
geometry and settings geometry at default and minimum window sizes.

At native app readiness, the dev-only driver also requires the handled Mesh-Talk
tray icon to exist and Tauri's unhandled default `main` tray icon to be absent.
This checks actual Tauri registration on each runner; it does not simulate an OS
tray click or inspect stale icons retained by a Windows shell after a crash.

The native menu's lifetime follows the registered tray resource. In the locked
Tauri 2.12.4 implementation, `TrayIconBuilder::menu` obtains an owned clone through
`Menu::inner_context_owned`; building the tray registers a clone in the app's
resource table and tray registry. The locked tray-icon 0.25.1 platform
implementations retain that owned menu, and muda 0.20.0 menu clones share their
native menu and child items. Dropping the local `tray_menu` variable therefore
does not release the registered menu. `TrayMenuState` retains the shared unread
item for updates; explicit tray removal or application exit ends registration.
These are dependency ownership guarantees, rather than native click evidence.
Recheck them when changing the locked versions: the owning sources are
[Tauri's tray builder and registration](https://docs.rs/tauri/2.12.4/src/tauri/tray/mod.rs.html),
[Tauri's owned menu clone](https://docs.rs/tauri/2.12.4/src/tauri/menu/menu.rs.html),
and the platform implementations in `tray-icon`'s `src/platform_impl`.

Each passing scenario supplies JSON observations, a screenshot and a redacted
process log. Schema 2 requires the tested source revision and SHA256 digests for
all three files. Validation requires typed scenario observations and fully decodes
bounded RGB/RGBA PNG pixels, rejecting corrupt, truncated, blank or transparent
screenshots. These checks prevent incomplete/stale evidence from passing; they
do not replace the scenario's interaction and layout assertions.

Publication evidence requires a committed clean checkout and a source-pinned
build from `node scripts/validation/native-build.mjs` before running the native
harness. Its ignored manifest binds actual Cargo executable records, binary
paths/digests and frontend assets to that revision. The runner verifies the proof
before and after evaluation. PR preparation rejects reports without successful
`cleanSource` and `buildVerified` observations; stale binaries cannot stand in
for a current build. See [the feedback loop](testing-feedback-loop.md#native-requirements-and-publication-evidence)
for commands. An uncommitted or unverified local run is development evidence; its
HEAD SHA identifies the base revision, not certification of that commit.

Keep the native build and its acceptance run inside one
[managed build lease](build-retention.md#commands). Cargo's compiler lock ends
before the example is launched; scheduled cache cleanup can otherwise remove
the example executable between those steps. For macOS or Linux, a single lease
can cover both commands:

```sh
python3 scripts/build-retention.py run -- bash -c \
  'node scripts/validation/native-build.mjs && node scripts/diagnostics/hidden-contacts.mjs'
```

On Windows, place both commands in the same managed PowerShell runner, retaining
the interactive desktop session and the configured D-drive state directory.

Native attachment evidence requires incoming manifest persistence, exact received
bytes and a rendered ready file card without a pending progress indicator. The
runner creates an isolated download directory and configures it through the
real owner-scoped settings API before reception. The OS save picker remains
outside the embedded DOM driver; the report marks picker-driven downloads as
unverified. Real CLI integration scenarios separately assert transferred bytes.

On macOS, avatar clearance is compared with actual AppKit standard-button bounds
converted to the content view. Other platforms report this macOS-specific chrome
observation as unavailable, not as successful native traffic-light coverage.

Native input requires an unlocked, active desktop session. A locked local macOS
session can still render WKWebView snapshots while refusing application/key-window
activation. That is an environment failure, not a successful interaction test;
retain focus diagnostics and rerun on an unlocked desktop or hosted runner.

## Portable browser execution

```sh
cd frontend
EVAL_BROWSER=chromium EVAL_TIER=core npx playwright test --config playwright.portable.config.ts
EVAL_BROWSER=webkit EVAL_TIER=extended npx playwright test --config playwright.portable.config.ts
```

Core scenarios cover both 760×520 and 1040×720 viewports without sharing
Linux-specific pixel baselines across operating systems. They check registration
and invalid-password responses, logout, bidirectional DM/channel rendering,
Unicode/multiline/wide content, interactive avatar/profile geometry, repeated
dialog Tab/Shift+Tab containment, light/Chinese interactions, and theme restoration
after reload. These remain mocked IPC tests, not Rust authentication assertions.

Every scenario attaches a screenshot, trace and source/platform/browser-bound
JSON. Page errors fail the test. A screenshot failure still leaves failed JSON;
a crashed page must not erase the failure record. Screenshot presence alone is
not the oracle: visible controls, content, hit targets, bounds and focus are asserted.

Extended runs exercise all existing palettes, locale changes, resize cycles and
repeated dialog opening. Their DOM-count assertion is a **DOM retention smoke**,
not native RSS, heap collection or a proof of absence of memory leaks. Profile
focus restoration to the avatar after dismissal is currently not established;
modal focus containment is tested separately and must not be presented as that
stronger accessibility guarantee.

UI CI keeps the full Linux regression suite and adds Chromium on three platforms
and macOS WebKit. Nightly/manual extended runs additionally require Linux Firefox.
The protected `Playwright UI E2E` aggregate requires every selected matrix; Firefox
is deliberately not selected for bounded PR core runs and cannot be skipped during
an extended run.

## CI gating and diagnostics

`Backend E2E` runs the real backend three-platform matrix and calls the reusable
native workflow. Its aggregate preserves the protected `Multi-process E2E` check
name and fails unless every backend and native platform succeeds. Failed, skipped
or missing child jobs cannot make this aggregate green. Artifacts are uploaded
on both success and failure with bounded retention.

Hosted macOS configures only the fixture multicast group as a local route and
requires a receive-capability probe before the real tests. This is runner setup,
not a product fallback or proof of multicast capability on an arbitrary LAN.

Inspect evidence before classifying a failure: environment startup/capability,
driver/input, assertion, protocol/delivery, persistence, and cleanup are distinct
failure categories. Publish which platforms/scenarios actually ran, which failed
and which were not run. A declared matrix is not an executed verification result.

## Manual desktop smoke

### Setup (two instances on one machine)

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

### Walk-through

1. **Register / sign in.** On the LoginScreen, use the **Register** tab to create a
   different account in each instance (e.g. `alice` / `bob`), then sign in. The app lands on
   the chat shell; no blank/black screen.
2. **Roster + presence.** Within a few seconds each instance lists the other in the
   **Sidebar** conversation list, with an identity glyph or received account photo and a
   presence dot. Use Up/Down/Home/End to move through conversations; selection and focus
   should remain visible. A running post office is tagged in Connection → Peers.
3. **DM.** Select the peer to open the **ConversationView** (crest header). Type in the
   **Composer** and send: the bubble appears immediately on the sender (optimistic) and
   shortly after on the recipient (live). Reply back the other way.
   - `Enter` sends; `Shift+Enter` inserts a newline. URLs render as clickable links.
4. **Channel.** Use **New channel**, confirm peer photos and device IDs in its picker, and
   invite a peer. On the peer device, confirm the channel is marked as a pending invitation,
   cannot compose, and offers **Accept invitation** and **Decline**. Accept, then exchange
   messages; the invitee must not see messages sent before the acceptance was observed.
   Open **Members** and confirm accepted/pending status, activity, and keyboard navigation.
   Transfer ownership to an accepted member, then leave from the former owner's device;
   its existing history should remain read-only. Hide the channel from the sidebar and
   restore it from **Hidden channels**; this must not change group membership.
   Confirm opening a member's DM returns focus to the conversation.
5. **React.** Hover a message and add an emoji reaction; the chip appears for both sides.
6. **@-mention.** In a channel, type `@` and pick a member from the autocomplete; the
   mention is highlighted, and the mentioned user sees a "mentioned you" marker.
7. **Reply.** Use the reply action on a message; the Composer shows a reply banner, and the
   sent message carries the parent snippet.
8. **Search.** Open the **Search** dialog, query a known word, click a result, and confirm
   it navigates into the right conversation.
9. **Files.** Expand the composer's **+** menu, attach a file, watch transfer progress, and
   confirm the recipient can save it. Open **Received files** to review attachments. The
   wallpaper should stay anchored across the sidebar, header, conversation, and composer
   when the menu opens or you switch chats.
10. **Verify contact.** Open **VerifyContact** for a DM peer; confirm the **safety number**
    matches on both instances, then mark the contact verified.
11. **Settings.** Open **Settings**: select each of the seven themes (dark, light, OLED,
    Argentina, Barcelona, Messi, Nature), confirm each personal theme's wallpaper spans
    the whole chat window, try several of Nature's 50 landscape wallpapers,
    toggle the optional conversation wallpaper, and switch
    among all six languages. Review privacy, retention, tray, autostart, notifications,
    download folder, and experimental call settings. At a 760×520 window, controls should
    remain reachable and focused sections should scroll into view.
12. **Connection.** Open **Connection**: Overview shows environment and identity; Peers
    lists the discovered roster with addresses; Logs and Troubleshoot tabs render.
13. **Pin.** Pin a contact; it moves into the **Pinned** section of the Sidebar.
14. **Durability.** Restart one instance, sign in, reopen the conversation → prior messages
    are still there (persisted history).
15. **Sign out.** Sign out → returns to the LoginScreen.
16. **Presentation and input.** Open the sticker picker and avatar gallery; scroll through
    presets and confirm offscreen thumbnails load when reached. Repeat key flows at the
    minimum window size, with long Unicode names and with reduced motion enabled.
17. **Privacy.** Switch among Balanced, Private, and Strict in **Privacy & security**;
    confirm the controls update and a manual change shows Custom. Lock the app, confirm
    background delivery continues, and unlock with a password. Enable Invisible mode,
    allow one account under Reply permissions, and verify that an unapproved account
    cannot start a DM. Hide a contact and confirm it leaves local lists while its
    existing history and shared channels remain available.
18. **Backup and restore.** In **Data and recovery**, create an encrypted backup and
    save its recovery key separately. Sign out, inspect the archive from **Restore
    from encrypted backup**, and restore it under a new local username. Check that
    the account fingerprint matches, the device fingerprint differs, and recovered
    channel history is read-only until a new invitation.

### Expected

- Peers appear by name/identity with presence dots; a post office is visible in Diagnostics.
- Messages, reactions, mentions, replies, and files flow both ways live.
- Safety numbers match between paired instances; verification sticks.
- Theme/language changes apply immediately; pinned and history survive restart.
- Privacy settings persist; a restored account has a new device identity and cannot
  send in archived channels until reinvited.
