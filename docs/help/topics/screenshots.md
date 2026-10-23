# Capture and annotate a screenshot

Choose an area, add annotations, and review the result in the composer before sending.

## Capture from a conversation

1. Open a conversation and expand the composer tools if they are collapsed.
2. Open **Screenshot** and choose the capture option, including hiding Mesh-Talk first if desired.
3. Follow any operating-system permission or capture prompt.
4. Select an area in the editor. Adjust its bounds and use the available drawing, text or mosaic tools as needed.
5. Confirm the edit to put the image in the composer. Review the intended conversation and press **Enter** or choose **Send** when ready.

Enter also sends a staged screenshot if capture returns focus to the page background. Shift+Enter and input-method confirmation do not send it; focused controls and open dialogs keep their own keyboard behavior.

Confirming an edit does not send it immediately. Cancel closes the editor without sending a screenshot. If sending fails, review the retained draft and error before retrying.

## Platform behavior

On macOS and Windows, capture uses the display containing the app window. Linux uses an image supplied through the desktop screenshot portal and edits that image inside the app. Portal prompts and availability depend on the desktop environment. macOS can require Screen Recording permission.

The entry is disabled when capture is unavailable. This feature does not provide scrolling screenshots, OCR, capture spanning multiple displays, or guaranteed redaction of the original screen. Review the exported region and annotations before sending.

## Further reference

[Platform permissions](https://github.com/OctopusGarage/mesh-talk/blob/main/specifications/platform_permissions.md)
