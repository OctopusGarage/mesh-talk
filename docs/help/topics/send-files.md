# Send and receive files

Attach a file in a conversation and control whether incoming transfers require approval.

## Send a file

1. Open the intended direct conversation or channel.
2. Use the composer's attachment control to choose a file. Expand the composer tools if they are collapsed.
3. Review the file card and any transfer status shown in the conversation.

An attachment card and completed local file bytes are separate things. A file can be visible in history while its contents are not available locally.

## Keep files across your devices

Open **Message Yourself** to keep a file for yourself, even before another device is linked. In **Your devices**, choose whether linked devices synchronize history, file cards and attachment contents. Contents require an eligible trusted device that still retains them and is reachable. Synchronizing a card alone does not copy the file bytes.

## Recover channel attachments

A channel member can supply retained attachments when both devices have accepted membership, the required keys and a reachable authorized path, and the serving member permits **Share a copy**. Members and post offices retain bounded copies; an expired, deleted or evicted copy cannot supply missing bytes. A file card or historical copy count does not guarantee a source remains available. See [Channels](channels.md) for sharing and catch-up behavior.

## Control incoming transfers

Open **More → Settings → Files & receiving** to choose the default receiving policy. Individual conversations can override it through their attachment receiving settings. Review the conversation's policy when its behavior differs from the default.

Receiving approval and the [download folder](download-location.md) are independent. Choosing a folder does not grant permission to receive a transfer.

After an attachment is accepted, Mesh Talk waits for its complete contents and
automatically saves it in the configured download folder, or the system Downloads
folder when no folder is configured. You do not need to press **Save** after
receiving it. Inline images keep their existing in-app preview behavior.

## Find received files

Use **Received files** in the sidebar to browse locally known file entries. If a transfer is waiting, check consent and connectivity before looking for a completed file on disk.

Completed downloads provide **Reveal**. If writing the download fails,
check that the destination is writable, then use the file's retry action.

## Copy text from an image

In the macOS desktop app, click an ordinary image in the conversation to enlarge
it. Move the pointer over the words, pause briefly, then drag to select the text.
Press **Command-C**, or right-click the selection and choose **Copy text**. Recognition
uses macOS Live Text and depends on the system's support for the image and language.
When zoomed in, hold **Option** while dragging to select text rather than pan.

## Image actions

Right-click an enlarged ordinary image to open the app's image menu:

- **Copy image** copies the whole decoded image as PNG, independent of zoom or pan.
  This action appears when the platform provides image clipboard support. If the
  clipboard refuses access, the app reports the failure; use **Save as…** instead.
- **Save as…** uses the app's file-saving dialog. Cancelling creates no saved file.
- **Reveal** appears after saving and opens the file's location.
- **Share image…** appears only when the platform reports file-sharing support.

The browser's new-window/download commands and automatic **Copy Subject** action
are not exposed. Copying a subject would remove its background; it is different
from copying the whole image and is not currently integrated.

Copied images and text follow your copy-confirmation and clipboard-clearing settings in
**Privacy & security**. Clipboard clearing is best-effort: if read access is denied
or the system has changed the PNG representation, the app leaves the clipboard
untouched rather than overwriting unknown content. Protected images do not allow text selection or copying.
Press **Escape** or use the close button to return to the conversation.

## Further reference

[File receiving settings](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md#receiving-and-saving-files)
