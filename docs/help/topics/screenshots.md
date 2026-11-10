# Capture and annotate a screenshot

Choose an area, add annotations, and review the result in the composer before sending.

## Capture from a conversation

1. Open a conversation and expand the composer tools if they are collapsed.
2. Open **Screenshot** and choose the capture option, including hiding Mesh-Talk first if desired.
3. Follow any operating-system permission or capture prompt.
4. Select an area in the editor. Adjust its bounds and use the available drawing, text or mosaic tools as needed.
5. Confirm the edit to copy the cropped, annotated image to the clipboard and put it in the composer. Review the intended conversation and press **Enter** or choose **Send** when ready.

Enter also sends a staged screenshot if capture returns focus to the page background. Shift+Enter and input-method confirmation do not send it; focused controls and open dialogs keep their own keyboard behavior.

Confirming an edit does not send it immediately. Cancel closes the editor without changing the clipboard or sending a screenshot. If clipboard copying fails, the editor keeps your selection and annotations so you can retry confirmation. If sending fails, review the retained draft and error before retrying.

## Keyboard shortcuts

Open **Settings → Shortcuts** to change the two capture bindings, clear a binding
or restore defaults. Initially, **Capture screen** uses Ctrl+Shift+X and **Hide
window & capture** uses Ctrl+Alt+Shift+X. On macOS use Command in place of Ctrl
and Option in place of Alt. Shortcuts work inside Mesh-Talk by default. Enable
**Use screenshot shortcuts globally** to activate them while using another app;
Mesh-Talk must remain running and unlocked. If a global shortcut is unavailable,
choose another binding or release it in the other app, then retry. Linux global
shortcuts require native X11; Wayland supports application shortcuts.

Hover over the screenshot entry or an editor icon to see its current shortcut.
Capture bindings are saved only on this device. Without a writable conversation,
activation shows the app and asks you to select a conversation first.

| Editor action | Key |
| --- | --- |
| Select an area | V |
| Rectangle | R |
| Ellipse | O |
| Arrow | A |
| Draw | P |
| Mosaic | M |
| Text | T |
| Undo annotation | Ctrl+Z / Command+Z |
| Confirm into the composer | Enter |
| Cancel | Esc |

Drawing tools require a selected area. Typing in a text annotation keeps normal
text editing; tool keys and annotation undo do not intercept that input. Open
dialogs, ongoing captures, IME composition and key repetition retain their own
keyboard behavior.

## Platform behavior

On macOS and Windows, capture uses the display containing the app window. Linux first uses an image supplied through the desktop screenshot portal. If that portal cannot provide screen capture, an X11 desktop can use the optional `xfce4-screenshooter` tool instead. Both paths open the same in-app editor. Portal prompts and availability depend on the desktop environment. macOS can require Screen Recording permission.

For Ubuntu XFCE sessions, including XFCE running inside WSL2, the X11 fallback requires `xfce4-screenshooter` on the app's executable search path. If it is missing, install it with `sudo apt install xfce4-screenshooter` and restart Mesh-Talk. The fallback captures the X11 desktop, which may include multiple displays; crop the desired area in the editor. It does not capture the surrounding Windows desktop. Native Wayland sessions require a working screenshot portal. Cancelling a portal prompt or receiving a capture error does not trigger a second capture method.

The entry is disabled when capture is unavailable. This feature does not provide scrolling screenshots, OCR, or guaranteed redaction of the original screen. Review the exported region and annotations before sending.

## Further reference

[Platform permissions](https://github.com/OctopusGarage/mesh-talk/blob/main/specifications/platform_permissions.md)
