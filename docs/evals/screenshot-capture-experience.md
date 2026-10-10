# Screenshot capture experience evaluation

2026-10-10. Scope: composer screenshot flow, native capture adapters, selection/annotation editor, and send/cancel behavior. This review separates code and mocked browser evidence from native runtime evidence.

| Criterion | Evidence | Result |
| --- | --- | --- |
| Deliberate send | Capture opens an editor; Send is disabled until a region is selected on Windows/Linux; macOS native selection preselects its returned region. Escape cancels. `frontend/e2e/chat.spec.ts` checks capture, selection, cropped PNG dimensions, send, and cancel. | Pass in browser E2E. |
| Annotation | Pen, rectangle, arrow, text, and undo are available in `ScreenshotEditor.tsx`. Export clips the selected pixels and paints annotations into the sent PNG. | Browser E2E exercises pen and verifies cropped PNG output; text/arrow/rectangle need broader interaction checks. |
| Failure safety | `screenshot_available` disables the control when no display/portal is detected. A failed temporary write keeps the editor and shows the error so the user can retry. | Pass in browser E2E; native unavailable states untested. |
| Platform integration | macOS `screencapture -i`; Windows xcap primary display; Linux XDG Screenshot portal. All return PNG/empty/error through the same command. | macOS Rust tests and Windows target `cargo check` pass. Linux portal API type-checks in an isolated build. Native Windows/Linux runtime validation remains required. |
| WeChat-like interaction | Area selection, annotation, explicit send, and Escape cancel are present. Windows/Linux expand the app window to show a frozen screen image while editing; macOS uses the native desktop selector before annotation. | Interaction sequence implemented. It is not a transparent overlay on the live desktop, and Windows currently offers the primary display only. |

## Release validation still needed

Run the native screenshot path from the actual installer on Windows 10/11 and on Linux GNOME Wayland, KDE Wayland, and X11. Check permission dialogs, portal absence, Escape, app hide/restore, fullscreen restoration, mixed-DPI displays, and a real recipient receiving the cropped annotated PNG. The Playwright browser mock cannot prove these OS behaviors. Keep the native release gate open until these scenarios pass.

## Cross-platform regression and AI review (2026-10-11)

The portable GitHub Actions UI matrix now includes `screenshot-portable.spec.ts` on Linux Chromium, Windows Chromium, macOS Chromium, and macOS WebKit. Its four scenarios cover selection and explicit send with PNG dimensions, annotation tools and undo, cancel without send, and unavailable/save-failure behavior. The portable evidence validator requires every scenario and checks that traces, screenshots, source revision, browser, and runner platform match; it marks this evidence as mocked IPC, not native capture. The separate three-OS health workflow builds and tests the platform-specific Rust code. Passing these jobs will not close the native release gate above.

Local evidence: the Chromium portable matrix passed and validated 26/26 scenarios; focused WebKit passed 4/4 screenshot scenarios. TypeScript, ESLint, the browser evidence validator tests, and deterministic AI contracts passed. A real Codex model run passed all 5 existing agent-contract cases. Those five cases evaluate agent instructions, not screenshot usability.

A separate `gpt-6-sol` visual review used the synthetic 1280×720 editor screenshot. It found the active crop boundary and exact send result ambiguous, and suggested visible labels for annotation and undo. This is a first-use hypothesis from pixels; the browser tests prove that the cropped PNG, annotation controls, and undo work, while the screenshot alone cannot establish discoverability or accessibility. The screenshot's 320×180 mock image is deliberately small and is not evidence of production image scaling.

The macOS visual check exposed a solid dark margin when a selected capture was smaller than the editor window. The stage now fills that unused space with a dimmed, blurred copy of the capture while the selected image stays sharp. A light synthetic capture in the portable E2E verifies the backdrop loads and keeps the cropped PNG assertion unchanged. A second model review confirmed the hard margin was gone but found the pale crop edge weak; the editor now draws a dark under-stroke below the dashed white selection and a fine edge around the sharp image. This is a presentation fix within the app window, not a transparent overlay on the desktop.
