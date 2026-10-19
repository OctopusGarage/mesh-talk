# Screenshot capture experience evaluation

2026-10-17. Scope: composer screenshot flow, native capture adapters, selection and annotation overlay, and confirmation/send/cancel behavior. Browser evidence uses mocked IPC and does not establish native OS behavior.

## Current interaction

On macOS and Windows, capture freezes the app window's display and opens the shared selection overlay. Linux uses the portal's chosen image in an in-app editor because the portal does not report a monitor identity. Drag to select an area; the green boundary has eight resize handles and displays dimensions in original image pixels. Drag inside the selection with the selection tool to move it. A compact white toolbar follows the selection and stays within the viewport. The toolbar offers selection, rectangle, ellipse, arrow, pen, mosaic, text, undo, cancel and confirmation. Annotation tools expose stroke widths and, except for mosaic, colors. Mosaic is painted into the exported PNG.

Enter or a double-click inside the selection with the selection tool confirms. IME Enter and Enter inside text inputs do not confirm. Focused toolbar buttons retain their normal Enter activation. Escape or right-click cancels. Confirmation adds an image preview to the originating conversation's composer; the user must press the normal Send action to transmit it. Message expiry is evaluated at that send. Failed sends retain the staged screenshot for retry. Export errors retain the overlay and selection. A conversation/account change invalidates the capture and staged screenshot. Acquisition is single-flight; cancellation or confirmation invalidates late acquisition results.

| Criterion | Implementation / focused evidence | Remaining boundary |
| --- | --- | --- |
| Deliberate send | Component and intake tests check confirmation without transmission, composer preview, explicit send, retry and stale ownership. Portable browser scenarios check the staged image and sent PNG dimensions. | Native delivery to a second installed client still needs validation. |
| Selection | Geometry and component tests cover normalization, movement bounds, corner resizing, handle hit testing, dimensions and cancellation. Retina browser scenarios check 2x image coordinates and bounded controls at 1280×720 and 760×620. | Mixed-DPI and native display scaling need installed-app validation. |
| Annotation | Separate annotation rendering owns pen, rectangle, ellipse, arrow, text and mosaic. Image-coordinate marks retain their chosen color/width. Selection borders, handles and dimensions are never exported. | Native image fidelity is not established by browser mocks. |
| Failure safety | Unavailable capture disables the entry point; export failures retain editing; write/send failures retain the composer draft. Window lifecycle tests cover existing fullscreen state, cancellation during expansion, overlapping editors and reopening during restoration. | OS permission dialogs and fullscreen transitions need runtime validation. |
| Platform integration | macOS captures the app window's monitor via `screencapture -x -R` after the existing Screen Recording check; the temporary file is removed by RAII. Windows matches the app window's monitor with xcap. Linux requests an image through the XDG Screenshot portal. | Linux may present its own portal UI before the shared overlay; this depends on the compositor. |
| Desktop overlay | On macOS and Windows the editor expands the main window temporarily. Linux keeps the portal image in the app window. Tauri simple fullscreen keeps macOS in its current Space; other platforms use regular fullscreen. Closing the last editor restores the previous fullscreen state; transitions are serialized across overlapping editors. | The implementation reuses the main window and is not an independent native capture panel. |

## Scope and native release validation

This implements the core WeChat-style selection, annotation and composer handoff. It does not claim complete feature or native parity. Automatic window snapping, scrolling screenshots, OCR, desktop pinning, clipboard export, save-to-file and selection spanning displays are outside this change. macOS and Windows resolve the app window's monitor before hiding it. Native multi-monitor and mixed-DPI alignment still need installed-app verification. Linux deliberately offers in-app editing of the portal image without promising alignment to the desktop beneath the app.

Run the installed app on macOS, Windows 10/11, Linux GNOME Wayland, KDE Wayland and X11. Check Screen Recording prompts and denial, missing portals, hiding/restoring the main window, Escape and right-click, existing fullscreen/maximized windows, cancellation during expansion, Retina/mixed-DPI displays and focus restoration. Confirm that the selected pixels, colors, mosaic and text reach a real recipient only after Send, and that expiry begins at Send. Keep the native release gate open until these pass.

## Regression evidence

`frontend/e2e/screenshot-portable.spec.ts` covers crop/annotation/explicit-send output, cancellation, fallback media IPC, undo, unavailable capture, failed-send retention and two Retina desktop layouts. `frontend/e2e/chat.spec.ts` retains its screenshot smoke checks with the new explicit composer send. `scripts/evals/browser-report.mjs` registers the portable scenarios and continues to require source/platform/browser evidence. These scenarios are mocked browser evidence, not native capture evidence.

Focused unit tests live in `ScreenshotEditor.test.ts`, `ComposerScreenshot.test.ts`, `attachments/conversationIntake.test.ts`, and `screenshot/{geometry,captureWindow}.test.ts`. Executed results are reported with the implementation handoff; this document describes test scope rather than treating historical passes as proof of the current revision.

## Evaluation follow-up — 2026-10-17

The full local UI run passed 336 cases with four default exclusions (two lite-edition cases, opt-in site capture and physical multi-machine media); no failures or flaky cases were reported. The focused WebKit screenshot run passed all seven scenarios. `make e2e` passed all 13 real backend cases, including the 110 MB directory case. The deterministic eval smoke and eight harness tests passed; the ten-case real-model workflow suite passed through Codex CLI.

An image-assisted source review and two follow-ups identified and closed five correctness issues: focused-button Enter handling, fullscreen ownership across overlapping editors, app-monitor capture on macOS/Windows, a documented Linux in-app portal-image fallback and single-flight/cancel-safe acquisition. The final source review reported no remaining scoped high/medium findings. These model judgments do not prove native OS behavior. The native release validation boundary above remains open.

The affected frontend unit run passed 63 tests, and both Rust screenshot unit tests passed. Type checking, lint, formatting, Clippy with all targets and the frontend production build passed. Local model reports are under `target/ai-eval/`; detailed untracked execution evidence is under `docs/superpowers/evidence/` in the feature worktree.
