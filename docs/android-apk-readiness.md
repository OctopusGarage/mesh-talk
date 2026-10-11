# Android APK readiness: conversation record

Status: **analysis only; no Android APK has been built or tested**. Recorded 2026-10-11
from the discussion about building on GitHub Actions and using Mesh-Talk on a phone.
Read this alongside [mobile-pwa-feasibility.md](mobile-pwa-feasibility.md) before resuming
mobile work. The PWA prototype and a native Android APK are different delivery paths.

## What exists on `feature/mobile-pwa`

- The branch has a phone-width, single-pane **browser/PWA** shell:
  `frontend/src/App.tsx` selects `frontend/src/features/mobile/MobileApp.tsx` at
  widths up to 768 px. It has a conversation list, a back button, a conversation
  screen, and reduced mobile settings. This is useful UI work, but it is not an APK
  or proof that the Rust/Tauri runtime runs on Android.
- The PWA path uses a browser node, WebRTC, and a desktop gateway. Its security,
  bootstrap, delivery, and dynamic-IP issues are recorded in the companion document.
- `src-tauri/gen/android/` and an Android build job do not exist. The current
  `.github/workflows/release.yml` builds desktop targets. The existing
  `src-tauri/tauri.conf.json` describes a desktop window and uses
  `com.mesh-talk.desktop` as its identifier.
- No physical Android device test, Android cross-compile, signed installable APK,
  or GitHub Actions APK artifact has been verified on this branch.

## Decisions from the discussion

1. **A GitHub-hosted runner can build and upload the APK.** Android Studio does not
   need to be installed on the user's local machine. The runner still needs Java,
   Android SDK/NDK, Rust Android targets, Node dependencies, and the Android project.
   Upload the APK as an Actions artifact first; publishing to Google Play is separate.
2. **CI packaging is the final build step, not the Android port.** A successful
   frontend build, or an APK that launches but cannot discover peers or send a DM,
   does not satisfy the user's phone-use goal.
3. **Proposed first native scope:** sign in, discover another device on the same
   Wi-Fi, send and receive text DMs while the app is open, and return from a
   conversation to the list. This is a proposed milestone, not an agreed decision
   that background delivery or attachments may be omitted permanently.
4. Preserve the core messaging, identity, and cryptographic semantics. Avoid
   changing transport solely to make a UI or build pipeline pass. Confirm whether
   the phone must link to the existing account and history, or may start as a new
   account, before calling it a mobile client.

## Native Android blockers and open work

| Priority | Evidence in this branch | Required work / proof |
| --- | --- | --- |
| Build | `src-tauri/src/lib.rs` has no `#[cfg_attr(mobile, tauri::mobile_entry_point)]` entry, and `src-tauri/gen/android/` is absent. | Add an Android entry point and initialize the Android project. Cross-compile on an Android target before adding release automation. |
| Build | `src-tauri/tauri.conf.json` has `identifier: com.mesh-talk.desktop`. Android application IDs cannot contain a hyphen. | Give Android a stable valid ID, for example `com.meshtalk.app`, using an Android-specific Tauri config if the desktop ID must stay. Do not change it after distributing builds. |
| Runtime | `src-tauri/src/lib.rs` unconditionally registers single-instance, window-state, autostart, and tray behavior, and assumes a desktop window. | Gate desktop plugins and window/tray lifecycle. Define Android foreground/background startup and shutdown behavior. |
| Runtime | `src-tauri/src/lib.rs::data_dir` uses `HOME`/`USERPROFILE` or a relative fallback; `src-tauri/src/session_store.rs` uses desktop keyring backends. | Use Android app-private paths and an Android-safe credential approach; verify login, restart, and account isolation on device. Do not silently store a password in browser storage. |
| Network | `crates/mesh-talk-core/src/transport/net.rs` joins UDP multicast; `crates/mesh-talk-core/src/discovery/service.rs` also runs frequent announce/rejoin and a `/24` unicast scan. | Validate Android Wi-Fi multicast reception, network permissions, TCP connectivity, Wi-Fi changes, battery cost, and recovery after suspension. Use a multicast lock only while needed if Android's Wi-Fi filter requires it. |
| Delivery | The node's networking is started from the Tauri app lifecycle. Android may suspend app work after it leaves the foreground. | Specify foreground-only behavior for the first milestone or design a justified Android background service and notification. Test screen-off, app switch, process death, and resume. |
| Files | `frontend/src/features/chat/ConversationView.tsx` passes picked paths to file commands; Rust file commands expect filesystem paths. Android's document picker can return content URIs. | Import picked content into app-private storage or stream it through an Android bridge; export received files through Android's document APIs. Test image, attachment, and save/open flows. |
| Platform controls | `frontend/src/lib/platform.ts::needsCustomWindowControls` treats every Tauri platform except macOS as a frameless desktop window. | Recognize Android explicitly so minimize/maximize/close chrome and drag regions do not appear on a phone. |

`capture_screen` on this branch already has a non-macOS error fallback in
`src-tauri/src/chat_commands.rs`; it is still a desktop screenshot feature and
should be hidden on Android unless replaced by an Android capture flow. The
newer development branch has a different screenshot implementation, so recheck
that file when porting fixes across branches.

## Phone interaction inventory

| Feature | Current branch state | Native APK treatment |
| --- | --- | --- |
| Conversation navigation | `MobileApp` already swaps list and conversation; the back button clears `active`. | Reuse the single-pane structure, and wire the Android system Back gesture/button. Test rotation and resuming an active conversation. |
| Window controls and sidebar resize | Desktop `WindowControls` and `Sidebar` still exist; platform detection would show window controls in Android Tauri. | Hide window controls and drag/resize affordances on Android. Keep resizing on desktop only. |
| Launch at login, close to tray, relay hosting, download folder | `MobileSettings` already omits them for phone width, while `SettingsDialog` gates some controls on `isTauri()` rather than desktop platform. | Keep them out of native Android even though Android Tauri returns `isTauri() === true`; use explicit platform/capability checks. |
| Screenshot, file attach, image picking | `MobileConversationScreen` passes only text/reply to `Composer`, so these buttons are already absent in the phone shell. | Keep screenshot and directory/file actions absent until Android import/export works. Add a phone image/file picker as a distinct tested milestone, rather than exposing path-based desktop actions. |
| Search, create channel, settings, dialogs | The mobile list contains these actions; shared dialogs and touch targets are not validated on Android. | Test at narrow widths with keyboard open, text scaling, safe areas, and touch. Fix any clipped dialog or sub-44 px target before release. |
| Message actions | Shared message bubbles and menus are reused by mobile. | Verify long-press/tap access to reply, react, copy, and delete on physical Android WebView; do not rely on hover or desktop right-click. |
| Pairing | Existing desktop linking uses a displayed QR/code; the PWA has a separate invite route. | Decide whether native phone is a linked device or a new account. If scanning is used, implement camera permission and trusted peer identity checks. |
| Calls and notifications | Desktop call and notification flows exist; the phone shell does not prove native camera/mic or background delivery. | Keep calls experimental/hidden until real-device media and permissions tests pass. Validate Android notification permission and delivery semantics separately. |

For a phone, hiding controls is appropriate only when the corresponding feature is
outside the stated first milestone. Core actions such as opening a conversation,
sending a message, and recognizing delivery state must remain usable.

## GitHub Actions build path, after the port

1. Commit the generated Android project and platform-specific configuration.
2. Add an Android job to `.github/workflows/release.yml` or a separate manual
   `workflow_dispatch` workflow. On a hosted runner, install Java, Android SDK/NDK,
   an Android Rust target (start with `aarch64-linux-android`), and `npm ci` in
   `frontend/`.
3. Run `cargo tauri android build --apk --target aarch64` from `src-tauri/`.
   Upload APKs from `src-tauri/gen/android/app/build/outputs/apk/` as workflow
   artifacts. Verify the APK's signature and installability on a phone; configure
   a protected signing key for distributable release builds. Do not commit keys.
4. Use an Android App Bundle (`--aab`) and release signing if Google Play
   publication becomes the goal. An APK artifact in Actions is not a Play release.

## Minimum evidence before claiming phone support

- Android build succeeds in CI; artifact is downloadable, signed as intended,
  installs, launches, and survives a restart without identity loss.
- On real phones, sign in and exchange text DMs with a desktop on ordinary Wi-Fi.
  Check peer discovery, actual message content, response, and delivery state.
- Repeat after Wi-Fi reconnect, desktop IP change, phone screen-off/resume, and
  app process restart. State explicitly whether messages can arrive in background.
- Exercise Android Back, soft keyboard, 320–400 px widths, landscape, safe areas,
  enlarged text, dialogs, and tap/long-press actions. Add file/notification/call
  tests only when those capabilities are in scope.
- Re-run the PWA-specific gates in [mobile-pwa-feasibility.md](mobile-pwa-feasibility.md)
  only if the PWA route remains a separate supported deliverable; an APK using the
  Rust core does not automatically fix or inherit the PWA gateway issues.

## References

- [Tauri mobile project structure](https://v2.tauri.app/start/project-structure/)
- [Tauri Android prerequisites](https://v2.tauri.app/start/prerequisites/#android)
- [Tauri Android APK/AAB builds](https://v2.tauri.app/distribute/google-play/#build-apks)
- [Tauri Android signing](https://v2.tauri.app/distribute/sign/android/)
- [Tauri Action mobile support](https://github.com/tauri-apps/tauri-action)
- [Android application ID rules](https://developer.android.com/build/configure-app-module)
- [Android Wi-Fi multicast lock](https://developer.android.com/reference/android/net/wifi/WifiManager.MulticastLock)
- [Android document access](https://developer.android.com/training/data-storage/shared/documents-files)
- [Android background work restrictions](https://developer.android.com/develop/background-work/background-tasks/bg-work-restrictions)
