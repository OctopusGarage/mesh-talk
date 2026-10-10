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
