# Contact privacy: native and usability evaluation

This evaluates issue #134's **local contact visibility**, not communication blocking.
Hiding must not remove raw discovery records, alter encrypted delivery, erase history,
or change existing shared groups. Policy is per signed-in local user on this device.
The same native harness also evaluates issue #135's separate **invisible mode**:
public presence suppression and account-scoped communication permissions. It restores
public mode before testing #134, so list hiding is never mistaken for blocking.

## Reproducible layers

| Layer | Entry point | What it proves |
| --- | --- | --- |
| Policy and state regressions | `cargo test -p mesh-talk --lib contact_policy` | Authorization, atomic persistence, corrupted storage, concurrent writes, lifecycle races and real registered IPC |
| Real mesh regression | `cargo test -p mesh-talk --lib contact_policy::native_mesh_tests` | Signed UDP discovery, encrypted TCP delivery in both directions, another device of the hidden account, unchanged existing history/roster, restart and offline restore |
| Invisible-mode core and runtime | `cargo test -p mesh-talk-core --lib node::privacy --features fast-test-kdf`; `cargo test -p mesh-talk-core --lib node::runtime::privacy_tests --features fast-test-kdf` | Admission, author/conversation projection, revocation, group/file scope, both-invisible restart and expired discovery routes; cached offline peers must not appear freshly online |
| Invisible-mode owner IPC | `cargo test -p mesh-talk --lib privacy_commands` | Real registered commands, durable policy and stale queued requests after logout |
| Browser interaction regressions | `cd frontend && npx playwright test e2e/hidden-contacts.spec.ts --workers=1 --retries=0` | Cancellation, disclosure, failure/retry, search/group projections, account isolation, keyboard and narrow/long-name layout; this layer intentionally mocks IPC |
| Invisible-mode browser interactions | `cd frontend && npx playwright test e2e/privacy.spec.ts --workers=1 --retries=0` | Mode confirmation, permission search/revocation, failure/retry, focus restoration and offline-account narrow layout; intentionally mocked IPC |
| Native WebView evaluation | `node scripts/diagnostics/hidden-contacts.mjs` after builds below | Actual registration/login, CLI peer discovery, GUI sending, inbound delivery while hidden, process restart, settings management, another local user and offline keyboard restore |
| Evidence gate | `node --test scripts/diagnostics/hidden-contacts-report.test.mjs` | Incomplete or failing native evaluations cannot be reported as passing |

Build the normal application and CLI first, then the dedicated evaluation example:

```bash
cd frontend && npm ci && npm run build
cd ..
cargo build --release --locked -p mesh-talk --bin mesh-talk -p mesh-talk-core --bin mesh-talk-node
cargo build --release --locked -p mesh-talk --example native-contact-eval
node scripts/diagnostics/hidden-contacts.mjs
```

On Linux, install the same WebKitGTK dependencies as CI and run the last command inside
`xvfb-run -a dbus-run-session --`; the keyboard helper also requires `xdotool`.
The `Hidden contacts native evaluation` Actions workflow
runs on Linux, Windows and macOS and uploads screenshots and JSON evidence even on failure.
`NATIVE_CONTACT_APP`, `NATIVE_CONTACT_NODE` and `NATIVE_CONTACT_OUTPUT` can select explicit
binary/output paths, including a shared local Cargo target directory. The native evidence
gate additionally requires `privacy-mode`, `privacy-reply` and `privacy-restart`:
actual mode confirmation/cancellation, manual permission after revocation, encrypted CLI
replies and delivery after a real application restart. Rejected traffic and both-private
restart and delivery after roster expiration are checked separately in core runtime
regressions, not inferred from screenshots. Cached routes are only connection hints:
fresh online presence requires a successful pinned authentication. Explicitly accepting
a known device's new account requires its signed binding and a separate new-account
permission; passive traffic must not transfer its old permission.

The example attaches a dev-only embedded WebDriver to the same desktop builder. Normal
release applications do not include the driver or enable its server. Test accounts and
configuration use isolated fixture roots; neither `HOME` nor `USERPROFILE` is reassigned.
The macOS driver activates only its own fixture window before animation and native
keyboard checks, then verifies actual document focus; it does not grant TCC permissions
or send global keyboard input. Run native desktop evaluations without competing GUI tests.
GitHub's macOS runner does not return same-host multicast with its default routing,
as independently measured by `native-multicast-probe.mjs` without product code.
Only the disposable macOS runner routes the single fixture group `224.0.0.167`
over loopback and requires a real UDP round trip before testing. This is native
same-host integration coverage, not proof
of discovery over every physical LAN. No workstation route is changed.
The example and CLI share one freshly allocated UDP port for real signed multicast
discovery, so test announcements and roster evidence do not mix with nearby production
users. The normal desktop entry point still uses `DEFAULT_DISCOVERY_PORT`. The fixture's
unique application identifier also isolates window-state and native WebView storage.
Cargo's existing example dev-dependencies enable `fast-test-kdf` for the example, but the
separately built CLI/application retain regular KDF settings. These runs assess behavior,
not production password-derivation latency or cryptographic strength.

The embedded driver's element commands operate inside the actual platform WebView.
Version 1.4 omits the `contextmenu` event from pointer actions and browser default behavior
from synthetic key events. The harness therefore dispatches the actual UI's `contextmenu`
event through standard WebDriver execute, and tests default keyboard behavior separately.
The same driver's `click_element` implementation (`src/platform/executor.rs`, version
1.4.0, lines 688–701) calls `click()` before `focus()`. For the two dialog-opening
triggers, including privacy confirmations, the harness uses standard execute to focus before clicking, matching
native mouse ordering and preventing focus from returning outside a newly opened modal.
The utility popover can remain open after its nested dialogs close. The harness checks
its actual `data-state` before opening it again, rather than toggling an already-open
parent and unmounting the nested Settings dialog.
Other element clicks retain the stock W3C command. Dialog animation completion and
native input presentation are observed before focus/layout checks; restoration requires
the actual saved backend policy to change, not merely a button disappearing.
On macOS a plugin present only in the evaluation example sends main-thread AppKit
`NSEvent` key-down/up events to that process's key window; it requires no Accessibility
permission changes and does not inject global OS input. Windows uses AppActivate with
the owned application PID and SendKeys; Linux selects the owned PID's visible window
with xdotool. These are native automation mechanisms, not physical keyboard participants.
Input failures remain failed scenarios; the harness does not substitute DOM activation
and report it as native keyboard behavior.
The CLI's `/account-msg <account-id> <text>` sends through the same account-addressed
API as the GUI. The harness reads the GUI's actual cryptographic `account_id` over
production IPC. Existing `/msg` remains device-addressed and writes a different
conversation; it cannot validate the GUI's account history.

## Usability rubric

Evaluate each property against assertions and screenshots, rather than treating a model's
score as proof of functionality:

| Property | Evidence |
| --- | --- |
| Discoverability | Contact context menu and Settings → Contacts → Manage are exercised |
| Informed choice | Confirmation explains all devices, local-user/device scope, retained history and continued messages/calls |
| Safe cancellation | Cancel receives initial focus; Enter cancels, Escape dismisses; contact remains visible |
| Reversibility | Searchable online and offline restoration, including keyboard activation |
| Lifecycle reliability | Real process restart retains policy and conversation; another local login has independent visibility |
| Honest failure states | Save/load failure and retry regressions preserve saved policy and do not briefly reveal hidden contacts |
| Layout/accessibility | Dialog labels, reachable controls, smallest native window and 390px browser layout; maximum-length saved name |
| Nonblocking semantics | A real hidden peer still appears in raw discovery and delivers encrypted messages to durable history |

The maximum-name regression initially exposed horizontal overflow: a grid child's automatic
minimum width prevented text truncation. Constraining the hidden-contact Tabs container with
`min-w-0` fixes that layout without changing unrelated dialogs. Keep the failing input as a
permanent regression case.

Inspect native screenshots as well as the automated results. Reports distinguish native
WebViews from browser mocks. A run is complete only when every required scenario and the
three-platform matrix pass; a build, empty report or skipped platform is not equivalent.

Invisible mode is not IP anonymity: a TCP port scan and Noise XX static keys remain
observable. Manually allowed trusted relays can carry only verified cached authors;
a new device without prior verified discovery/direct contact cannot deliver offline
through a legacy relay. Unknown sender proofs fail closed. Permissions are local to
the signed-in user/device, not synced to other devices. A commit already authorized
before logout may finish only in the old user's locked runtime namespace; queued
operations are rejected when the session generation changes.

This is a repeatable scenario evaluation plus visual inspection, **not a human participant
study**, screen-reader certification, exhaustive hardware/network compatibility claim, or
model-backed AI eval. The existing `make ai-eval` suite scores agent/workflow prompts and
does not evaluate this feature's UX. Human participant feedback remains a separate research
activity; it must not be reported as performed by automation.
