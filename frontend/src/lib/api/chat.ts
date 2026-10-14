import { invoke } from "@tauri-apps/api/core";

export type SyncCategory =
  | "history"
  | "attachments"
  | "profile"
  | "appearance"
  | "contacts";

export type DeviceSyncPolicy = Record<SyncCategory, boolean>;
import type {
  AccountInfo,
  ChannelInfo,
  ChannelMembersInfo,
  ChannelActivityInfo,
  HistoryItem,
  FileStatus,
  OutgoingFileStatus,
  PeerInfo,
  ReactionInfo,
  SafetyNumber,
  SearchHitInfo,
  TrustInfo,
} from "../types";

// Preserve order across rapid unread/read updates and webview reloads.
let lastBadgeRevision = 0;
function nextBadgeRevision(): number {
  lastBadgeRevision = Math.max(lastBadgeRevision + 1, Date.now() * 1000);
  return lastBadgeRevision;
}

export const chat = {
  getDeviceSyncPolicy: () =>
    invoke<DeviceSyncPolicy>("get_device_sync_policy"),
  setDeviceSyncCategory: (category: SyncCategory, enabled: boolean) =>
    invoke<DeviceSyncPolicy>("set_device_sync_category", {
      category,
      enabled,
    }),
  ownerIdentity: (owner: string) =>
    invoke<{ owner: string; device_id: string; account_id: string }>(
      "owner_node_identity",
      { owner },
    ),
  enqueueText: (
    owner: string,
    account: string,
    text: string,
    replyTo: string | null,
  ) => invoke<string>("owner_enqueue_text", { owner, account, text, replyTo }),
  enqueueSticker: (
    owner: string,
    account: string,
    stickerId: string,
    fallback: string,
  ) =>
    invoke<string>("owner_enqueue_sticker", {
      owner,
      account,
      stickerId,
      fallback,
    }),
  enqueueFile: (
    owner: string,
    account: string,
    path: string,
    media: boolean,
    progressKey?: string,
  ) =>
    invoke<{ id: string; fileConv: string }>("owner_enqueue_file", {
      owner,
      account,
      path,
      media,
      ...(progressKey ? { progressKey } : {}),
    }),
  ownerHistory: (owner: string, account: string, limit: number) =>
    invoke<HistoryItem[]>("owner_account_history", { owner, account, limit }),
  deliveryStatuses: (owner: string, account: string, ids: string[]) =>
    invoke<
      Array<{
        id: string;
        status: "awaiting" | "delivered";
        file_progress?: OutgoingFileStatus;
      }>
    >("owner_delivery_statuses", { owner, account, ids }),
  myId: () => invoke<string>("my_id"),
  accountId: () => invoke<string>("account_id"),

  /** Set the OS app-icon unread badge (dock/taskbar); 0 clears it. */
  setBadge: (count: number, owner: string) =>
    invoke<void>("set_badge", { count, owner, revision: nextBadgeRevision() }),
  setNotificationContext: (
    activePeerDevices: string[],
    activeChannel: string | null,
  ) =>
    invoke<void>("set_notification_context", {
      activePeerDevices,
      activeChannel,
      revision: nextBadgeRevision(),
    }),

  /** The current Wi-Fi network name (SSID), or null if unknown (wired / no Wi-Fi / OS withholds it). */
  networkName: () => invoke<string | null>("network_name"),

  listPeers: () => invoke<PeerInfo[]>("list_peers"),
  listAccounts: () => invoke<AccountInfo[]>("list_accounts"),
  listChannels: () => invoke<ChannelInfo[]>("list_channels"),

  // Message lifecycle (delete is local; recall propagates within the recall window; clear
  // wipes local history). `convId` is the channel id when `isChannel`, else the peer account id.
  deleteMessage: (convId: string, target: string, isChannel: boolean) =>
    invoke<void>("delete_message", { convId, target, isChannel }),
  recallMessage: (convId: string, target: string, isChannel: boolean) =>
    invoke<void>("recall_message", { convId, target, isChannel }),
  clearConversation: (convId: string, isChannel: boolean) =>
    invoke<void>("clear_conversation", { convId, isChannel }),
  // Send an animated sticker as its own message. `fallback` is the emoji char shown if the
  // recipient lacks that bundled sticker.
  sendSticker: (
    convId: string,
    stickerId: string,
    fallback: string,
    isChannel: boolean,
  ) => invoke<void>("send_sticker", { convId, stickerId, fallback, isChannel }),

  // Direct messages (device-addressed)
  sendDm: (recipient: string, text: string, replyTo: string | null = null) =>
    invoke<void>("send_dm", { recipient, text, replyTo }),
  history: (peer: string, limit: number) =>
    invoke<HistoryItem[]>("history", { peer, limit }),
  reactions: (peer: string) => invoke<ReactionInfo[]>("reactions", { peer }),
  reactDm: (
    recipient: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_dm", { recipient, target, emoji, remove }),
  sendFileDm: (recipient: string, path: string, media: boolean) =>
    invoke<string>("send_file_dm", { recipient, path, media }),

  // Account-addressed (multi-device) messages
  sendToAccount: (
    account: string,
    text: string,
    replyTo: string | null = null,
  ) => invoke<void>("send_to_account", { account, text, replyTo }),
  accountHistory: (account: string, limit: number) =>
    invoke<HistoryItem[]>("account_history", { account, limit }),
  accountReactions: (account: string) =>
    invoke<ReactionInfo[]>("account_reactions", { account }),
  reactAccount: (
    account: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_account", { account, target, emoji, remove }),
  sendFileToAccount: (account: string, path: string, media: boolean) =>
    invoke<string>("send_file_to_account", { account, path, media }),

  // Channels
  createChannel: (name: string, memberIds: string[]) =>
    invoke<string>("create_channel", { name, memberIds }),
  channelMembers: (channelId: string) =>
    invoke<ChannelMembersInfo>("channel_members", { channelId }),
  setChannelReplicaEnabled: (channelId: string, enabled: boolean) =>
    invoke<void>("set_channel_replica_enabled", { channelId, enabled }),
  channelCustodyStatuses: (channelId: string, ids: string[]) =>
    invoke<Array<{ id: string; replicas: number }>>(
      "channel_custody_statuses",
      {
        channelId,
        ids,
      },
    ),
  channelActivity: (channelId: string) =>
    invoke<ChannelActivityInfo[]>("channel_activity", { channelId }),
  addChannelMember: (channelId: string, memberId: string) =>
    invoke<void>("add_channel_member", { channelId, memberId }),
  removeChannelMember: (channelId: string, memberId: string) =>
    invoke<void>("remove_channel_member", { channelId, memberId }),
  transferChannelOwner: (channelId: string, memberId: string) =>
    invoke<void>("transfer_channel_owner", { channelId, memberId }),
  leaveChannel: (channelId: string) =>
    invoke<void>("leave_channel", { channelId }),
  acceptChannelInvite: (channelId: string) =>
    invoke<void>("accept_channel_invite", { channelId }),
  renameChannel: (channelId: string, name: string) =>
    invoke<void>("rename_channel", { channelId, name }),
  dissolveChannel: (channelId: string) =>
    invoke<void>("dissolve_channel", { channelId }),
  sendChannelMessage: (
    channelId: string,
    text: string,
    replyTo: string | null = null,
  ) => invoke<void>("send_channel_message", { channelId, text, replyTo }),
  channelHistory: (channelId: string, limit: number) =>
    invoke<HistoryItem[]>("channel_history", { channelId, limit }),
  channelReactions: (channelId: string) =>
    invoke<ReactionInfo[]>("channel_reactions", { channelId }),
  reactChannel: (
    channelId: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_channel", { channelId, target, emoji, remove }),
  sendFileChannel: (
    channelId: string,
    path: string,
    media: boolean,
    progressKey?: string,
  ) =>
    invoke<string>("send_file_channel", {
      channelId,
      path,
      media,
      ...(progressKey ? { progressKey } : {}),
    }),

  // Contact trust / safety numbers
  getTrust: (owner: string, accountId: string, currentFingerprint: string) =>
    invoke<TrustInfo>("get_trust", { owner, accountId, currentFingerprint }),
  markVerified: (owner: string, accountId: string, fingerprint: string) =>
    invoke<void>("mark_verified", { owner, accountId, fingerprint }),
  safetyNumber: (fingerprint: string) =>
    invoke<SafetyNumber>("safety_number", { fingerprint }),

  // Files + search + device linking
  saveFile: (fileConv: string, dest: string) =>
    invoke<void>("save_file", { fileConv, dest }),
  fileStatuses: (fileConvs: string[]) =>
    invoke<FileStatus[]>("file_statuses", { fileConvs }),
  mediaMigrationWarning: () => invoke<boolean>("media_migration_warning"),
  attachmentReceivePolicy: (account?: string) =>
    invoke<import("../types").AttachmentReceivePolicy>(
      "attachment_receive_policy",
      { account: account ?? null },
    ),
  setAttachmentGlobalAuto: (auto: boolean) =>
    invoke<void>("set_attachment_global_auto", { auto }),
  setAttachmentContactAuto: (account: string, auto: boolean | null) =>
    invoke<void>("set_attachment_contact_auto", { account, auto }),
  decideAttachment: (fileConv: string, accept: boolean) =>
    invoke<void>("decide_attachment", { fileConv, accept }),
  saveFileToDir: (fileConv: string, dir: string) =>
    invoke<string>("save_file_to_dir", { fileConv, dir }),
  /** The platform's standard Downloads folder (macOS ~/Downloads, Windows Downloads, Linux
   * XDG_DOWNLOAD_DIR → ~/Downloads). The default save location when none is configured. */
  defaultDownloadDir: () => invoke<string | null>("default_download_dir"),
  openDownloadDir: (owner: string) =>
    invoke<void>("open_download_dir", { owner }),
  readFile: (fileConv: string) =>
    invoke<ArrayBuffer>("read_file", { fileConv }),
  /** Read DURABLE media bytes (image/screenshot/video) from the chat-media store. Survives
   * chunk prune + restart, unlike readFile (transient chunks). Rejects if none is stored. */
  readMedia: (fileConv: string) =>
    invoke<ArrayBuffer>("read_media", { fileConv }),
  /** Write picked/pasted bytes to a temp file and return its path, to feed the file-send
   *  pipeline. `name` (the picker has one) is kept verbatim so the real filename + extension
   *  survive; without it a `pasted-<ts>.<ext>` name is synthesized for clipboard bytes. */
  writeTempFile: (bytes: number[], ext: string, name?: string) =>
    invoke<string>("write_temp_file", { bytes, ext, name }),
  deleteTempFile: (path: string) => invoke<void>("delete_temp_file", { path }),
  /**
   * Capture a screenshot as PNG bytes. When `hideWindow` is true the app window is hidden
   * during the capture and restored after. Resolves to empty bytes if the user cancels.
   */
  captureScreen: (hideWindow: boolean) =>
    invoke<number[]>("capture_screen", { hideWindow }).then(
      (b) => new Uint8Array(b),
    ),
  screenshotAvailable: () => invoke<boolean>("screenshot_available"),
  search: (query: string) => invoke<SearchHitInfo[]>("search", { query }),
  startLinking: () => invoke<string>("start_linking"),
  stopLinking: () => invoke<void>("stop_linking"),
  linkDevice: (peer: string, code: string) =>
    invoke<string>("link_device", { peer, code }),
  rekeyAccount: () => invoke<string>("rekey_account"),
};
