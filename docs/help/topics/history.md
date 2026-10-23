# Search and retain message history

Search available local conversations and choose how long this device keeps history.

## Search available history

Use the sidebar search for messages across available conversations, or open the history control in a conversation header to search that conversation. Results reflect the locally available history and the app's visibility and expiry rules.

Sidebar search shows its recent-message window and tells you when more matches
exist. Selecting a result opens that exact message, including a bounded context
view when it is outside the current live history window. Use the return-to-latest
control to resume the live conversation. A message that has expired, been
recalled or become unavailable shows an explanation; matching text elsewhere
does not substitute for the selected message.

## Read delayed messages

Group messages can arrive after reconnecting through another authorized member or a post office. Their position follows retained conversation history rather than download time. While you read older content, the view keeps your place; use **New messages** or **Earlier messages received** to move to unread content.

Opening a conversation does not mark every offscreen message read. Newly recovered content can remain unread even when it was authored before messages you already read. Catch-up notifications summarize recovered messages rather than treating every row as a new live alert. If the app cannot open a tracked message or asks you to review more unread history, use the offered review or explicit mark-read action. See [Channels](channels.md) for recovery and sharing limits.

## Keep an unfinished message

Text drafts stay with their conversation when you switch between contacts, groups
and personal notes. Other conversations show a red **[Draft]** label and preview;
opening the conversation restores the original text, including line breaks.

Drafts are encrypted on this device and restore after restarting and signing in
as the same local user. Saving happens after ten seconds without typing; continued typing restarts
the idle timer. Switching conversations or leaving the
window also requests a save. Quitting abruptly can lose recent edits. Drafts do
not sync to other devices and do not include attachments or reply targets. Protected
drafts restore their protection mode with their text and show a concealed sidebar
preview. Protected text remains a draft until local durable acceptance; a rejected
send keeps both its text and protection mode for retry.
Sending or clearing the text removes its draft. A save error keeps your text in
the current window and offers **Retry**.

## Change retention

1. Open **More → Settings → History & backup**.
2. Review the local history retention setting.
3. Before shortening retention, read the confirmation: eligible history is deleted immediately.
4. Choose **Create a backup first** if you want to preserve a recoverable copy before changing it.

Cancel keeps the previous retention value. Increasing retention later cannot restore deleted history. Other devices keep their own retained data; changing this setting does not guarantee remote deletion.

Local retention and per-message expiry are separate. See [Delivery](delivery.md) and [Backup](backup.md).

## Further reference

[Retention and backup settings](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md#retention-and-backup)
