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

## Delete a chat or its history

When leaving a group, choose whether to also delete its history and remove the
chat from your linked devices. The option starts unchecked. If the group leave
is still waiting or fails, the history is not deleted. After the account leaves,
**Cleaning up history** can remain visible until cleanup is confirmed. The app
continues checking the original request; leaving the group and completing history
cleanup are separate steps. Closing the dialog does not cancel an account action
that has already been accepted.

If you kept the history, open the exited group's **Delete chat** action in its
chat footer or sidebar menu later. Confirming permanently removes that chat and
its history from your linked devices. This does not dissolve the group or remove
other members' copies. Favorites and files you exported are kept.

Linked devices apply the account change when they next synchronize. An offline
or outdated client may still show old data until it reconnects or is updated.
The completion shown here confirms this device's cleanup, not an acknowledgment
from every offline linked device.

**Delete contact** removes a contact from your linked devices and offers the
same optional history deletion. A contact can be removed while online. Removal
does not block them: a new incoming message or explicitly reopening the contact
can make the chat visible again. Deleted history stays deleted; ordinary
presence updates do not restore the contact.

## Keep an unfinished message

Text drafts stay with their conversation when you switch between contacts, groups
and personal notes. Other conversations show a red **[Draft]** label and preview;
opening the conversation restores the original text, including line breaks.

Drafts are encrypted on this device and restore after restarting and signing in
as the same local user. Saving happens after ten seconds without typing; continued typing restarts
the idle timer. Switching conversations or leaving the
window also requests a save. Quitting abruptly can lose recent edits. Drafts synchronize with certified linked devices when history sync is enabled.
Attachments and reply targets are not draft content. Concurrent edits retain
the local unsaved text until the version conflict is resolved; clearing or sending
a draft publishes a deletion record so an older device cannot restore it. Protected
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
