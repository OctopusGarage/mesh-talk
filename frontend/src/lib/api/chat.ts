import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";

/** Local scheduling facts only; absence means unknown. */
export interface DeliveryObservation {
  kind: "attempting" | "retry_scheduled";
  age_ms: number;
  reason?: "post_office_disabled" | "no_known_route" | "attempt_timed_out";
}

export type SyncCategory =
  "history" | "attachments" | "profile" | "appearance" | "contacts";

export type DeviceSyncPolicy = Record<SyncCategory, boolean>;

export interface AccountPreference {
  category: "appearance" | "contacts";
  key: string;
  value: string | null;
  revision: number;
  device_id: string;
}

export interface AccountLoginSyncStatus {
  account_id: string;
  available_revision: number | null;
  applied_revision: number | null;
  devices: Array<{
    device_id: string;
    phase:
      | "synchronizing"
      | "up_to_date"
      | "waiting_for_source"
      | "retry_scheduled"
      | "unsupported"
      | "permission_denied";
    revision: number;
    failure:
      | "connection"
      | "response_closed"
      | "unsupported_protocol"
      | "invalid_record"
      | "permission_denied"
      | "storage"
      | "timeout"
      | null;
    attempts: number;
    retry_delay_ms: number;
  }>;
}

export interface DeviceSyncStatus {
  device_id: string;
  phase:
    | "offline"
    | "catching_up"
    | "up_to_date"
    | "waiting_for_files"
    | "error"
    | "disabled";
  pending_attachments: number;
  /** Older cores omit this; V1 peers cannot separate file cards from file bytes. */
  legacy_file_metadata?: boolean;
  document_phase?: DeviceSyncStatus["phase"];
  document_error?: string | null;
  document_requires_upgrade?: boolean;
  error: string | null;
}
import type {
  AccountInfo,
  ChannelInfo,
  ChannelMetadata,
  ChannelMembersInfo,
  ChannelHistoryPolicy,
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

/** Namespace is part of identity; a legacy device is never an account ID. */
export interface SearchReference {
  kind: "account" | "channel" | "legacy-device";
  target: string;
  message_id: string | null;
}
export interface ReferencedSearchHit extends SearchHitInfo {
  reference: SearchReference;
  expires_at_ms: number | null;
}
export interface RecentSearch {
  hits: ReferencedSearchHit[];
  coverage: {
    scope: "recent-window";
    per_conversation_limit: number;
    result_limit: number;
    truncated: boolean;
  };
}
export interface SearchContext {
  status: "found" | "unavailable";
  reference: SearchReference;
  messages: HistoryItem[];
  newer_entries: number | null;
}

// Preserve order across rapid unread/read updates and webview reloads.
let lastBadgeRevision = 0;
function nextBadgeRevision(): number {
  lastBadgeRevision = Math.max(lastBadgeRevision + 1, Date.now() * 1000);
  return lastBadgeRevision;
}

// Tauri's normal IPC transport returns raw Response bytes as an ArrayBuffer. If it falls
// back to postMessage (for example when the custom IPC protocol is blocked), it serializes
// the same bytes as a JSON number array. Blob([numberArray]) would encode comma-separated
// text instead of the image, so normalize at the API boundary.
function mediaBytes(bytes: ArrayBuffer | number[]): ArrayBuffer {
  return Array.isArray(bytes) ? new Uint8Array(bytes).buffer : bytes;
}

export const chat = {
  revealMessage: (
    owner: string,
    reference: {
      kind: "account" | "channel";
      target: string;
      message_id: string;
    },
    password?: string,
  ) =>
    invoke<{ text: string; visible_for_ms: number }>("owner_reveal_message", {
      owner,
      reference,
      password: password ?? null,
    }),
  revealImage: (
    owner: string,
    reference: {
      kind: "account" | "channel";
      target: string;
      message_id: string;
    },
    password?: string,
  ) =>
    invoke<ArrayBuffer | number[]>("owner_reveal_image", {
      owner,
      reference,
      password: password ?? null,
    }).then(mediaBytes),
  getAccountLoginSyncStatus: (owner: string) =>
    invoke<AccountLoginSyncStatus>("get_account_login_sync_status", { owner }),
  getDeviceSyncPolicy: (owner: string) =>
    invoke<DeviceSyncPolicy>("get_device_sync_policy", { owner }),
  getDeviceHostnames: (owner: string, expectedAccount: string) =>
    invoke<Record<string, string>>("get_device_hostnames", {
      owner,
      expectedAccount,
    }),
  getDeviceSyncStatus: (owner: string) =>
    invoke<DeviceSyncStatus[]>("get_device_sync_status", { owner }),
  setDeviceSyncCategory: (
    category: SyncCategory,
    enabled: boolean,
    owner: string,
  ) =>
    invoke<DeviceSyncPolicy>("set_device_sync_category", {
      category,
      enabled,
      owner,
    }),
  getAccountPreferences: (category: "appearance" | "contacts", owner: string) =>
    invoke<AccountPreference[]>("get_account_preferences", { category, owner }),
  seedAccountPreference: (
    category: "appearance" | "contacts",
    key: string,
    value: string | null,
    owner: string,
    expectedAccount: string,
  ) =>
    invoke<AccountPreference>("seed_account_preference", {
      category,
      key,
      value,
      owner,
      expectedAccount,
    }),
  setAccountPreference: (
    category: "appearance" | "contacts",
    key: string,
    value: string | null,
    owner: string,
    expectedAccount: string,
  ) =>
    invoke<AccountPreference>("set_account_preference", {
      category,
      key,
      value,
      owner,
      expectedAccount,
    }),
  ownerStartupIdentity: (owner: string) =>
    invoke<{
      owner: string;
      startup_id: string;
      phase: "pending" | "identity" | "ready" | "failed";
      account_id?: string;
      device_id?: string;
    } | null>("owner_startup_identity", { owner }),
  ownerIdentity: (owner: string) =>
    invoke<{
      owner: string;
      device_id: string;
      account_id: string;
      display_name?: string;
      history_revision?: number;
      supports_push_state?: boolean;
    }>("owner_node_identity", { owner }),
  enqueueText: (
    owner: string,
    account: string,
    text: string,
    replyTo: string | null,
    expiresAtMs?: number | null,
    protection?: import("../types").MessageProtection | null,
  ) =>
    invoke<string>("owner_enqueue_text", {
      owner,
      account,
      text,
      replyTo,
      expiresAtMs,
      ...(protection ? { protection } : {}),
    }),
  enqueueSticker: (
    owner: string,
    account: string,
    stickerId: string,
    fallback: string,
    expiresAtMs?: number | null,
  ) =>
    invoke<string>("owner_enqueue_sticker", {
      owner,
      account,
      stickerId,
      fallback,
      expiresAtMs,
    }),
  enqueueFile: (
    owner: string,
    account: string,
    path: string,
    media: boolean,
    progressKey?: string,
    expiresAtMs?: number | null,
    protection?: import("../types").MessageProtection | null,
  ) =>
    invoke<{ id: string; fileConv: string }>("owner_enqueue_file", {
      owner,
      account,
      path,
      media,
      expiresAtMs,
      ...(progressKey ? { progressKey } : {}),
      ...(protection ? { protection } : {}),
    }),
  enqueueChannelText: (
    owner: string,
    channelId: string,
    text: string,
    replyTo: string | null,
    expiresAtMs?: number | null,
    protection?: import("../types").MessageProtection | null,
  ) =>
    invoke<string>("owner_enqueue_channel_text", {
      owner,
      channelId,
      text,
      replyTo,
      expiresAtMs,
      ...(protection ? { protection } : {}),
    }),
  enqueueChannelSticker: (
    owner: string,
    channelId: string,
    stickerId: string,
    fallback: string,
    expiresAtMs?: number | null,
  ) =>
    invoke<string>("owner_enqueue_channel_sticker", {
      owner,
      channelId,
      stickerId,
      fallback,
      expiresAtMs,
    }),
  enqueueChannelFile: (
    owner: string,
    channelId: string,
    path: string,
    media: boolean,
    progressKey: string,
    expiresAtMs?: number | null,
    protection?: import("../types").MessageProtection | null,
  ) =>
    invoke<{ id: string; fileConv: string }>("owner_enqueue_channel_file", {
      owner,
      channelId,
      path,
      media,
      progressKey,
      expiresAtMs,
      ...(protection ? { protection } : {}),
    }),
  ownerHistory: (owner: string, account: string, limit: number) =>
    invoke<HistoryItem[]>("owner_account_history", { owner, account, limit }),
  deliveryStatuses: (owner: string, account: string, ids: string[]) =>
    invoke<
      Array<{
        id: string;
        status: "awaiting" | "delivered";
        observation?: DeliveryObservation;
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
    expiresAtMs?: number | null,
  ) =>
    invoke<void>("send_sticker", {
      convId,
      stickerId,
      fallback,
      isChannel,
      expiresAtMs,
    }),

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
  channelMetadata: (
    owner: string,
    expectedAccount: string,
    channelId: string,
  ) =>
    invoke<ChannelMetadata>("channel_metadata", {
      owner,
      expectedAccount,
      channelId,
    }),
  setChannelCover: (
    owner: string,
    expectedAccount: string,
    channelId: string,
    content: string | null,
    expectedRevision: string | null,
  ) =>
    invoke<ChannelMetadata>("set_channel_cover", {
      owner,
      expectedAccount,
      channelId,
      content,
      expectedRevision,
    }),
  setChannelAnnouncement: (
    owner: string,
    expectedAccount: string,
    channelId: string,
    content: string | null,
    expectedRevision: string | null,
  ) =>
    invoke<ChannelMetadata>("set_channel_announcement", {
      owner,
      expectedAccount,
      channelId,
      content,
      expectedRevision,
    }),
  createChannel: (name: string, memberIds: string[]) =>
    invoke<string>("create_channel", { name, memberIds }),
  channelMembers: (channelId: string) =>
    invoke<ChannelMembersInfo>("channel_members", { channelId }),
  setChannelHistoryPolicy: (channelId: string, policy: ChannelHistoryPolicy) =>
    invoke<void>("set_channel_history_policy", { channelId, policy }),
  setChannelReplicaEnabled: (channelId: string, enabled: boolean) =>
    invoke<void>("set_channel_replica_enabled", { channelId, enabled }),
  setGroupFileSharingOverride: (
    owner: string,
    networkCost: "unknown" | "unrestricted" | "restricted",
    enabled: boolean,
  ) =>
    invoke<void>("set_group_file_sharing_override", {
      owner,
      networkCost,
      enabled,
    }),
  channelCustodyStatuses: (channelId: string, ids: string[]) =>
    invoke<Array<{ id: string; replicas: number; post_offices?: number }>>(
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
  removeChannelAccountMembers: (channelId: string, accountId: string) =>
    invoke<void>("remove_channel_account_members", { channelId, accountId }),
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
    expiresAtMs?: number | null,
  ) =>
    invoke<void>("send_channel_message", {
      channelId,
      text,
      replyTo,
      expiresAtMs,
    }),
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
    expiresAtMs?: number | null,
  ) =>
    invoke<string>("send_file_channel", {
      channelId,
      path,
      media,
      expiresAtMs,
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
  saveFileToDir: (fileConv: string, dir: string, expectedOwner?: string) =>
    invoke<string>("save_file_to_dir", {
      fileConv,
      dir,
      ...(expectedOwner ? { expectedOwner } : {}),
    }),
  /** The platform's standard Downloads folder (macOS ~/Downloads, Windows Downloads, Linux
   * XDG_DOWNLOAD_DIR → ~/Downloads). The default save location when none is configured. */
  defaultDownloadDir: () => invoke<string | null>("default_download_dir"),
  openDownloadDir: (owner: string) =>
    invoke<void>("open_download_dir", { owner }),
  readFile: (fileConv: string) =>
    invoke<ArrayBuffer | number[]>("read_file", { fileConv }).then(mediaBytes),
  /** Read DURABLE media bytes (image/screenshot/video) from the chat-media store. Survives
   * chunk prune + restart, unlike readFile (transient chunks). Rejects if none is stored. */
  readMedia: (fileConv: string) =>
    invoke<ArrayBuffer | number[]>("read_media", { fileConv }).then(mediaBytes),
  /** Write picked/pasted bytes to a temp file and return its path, to feed the file-send
   *  pipeline. `name` (the picker has one) is kept verbatim so the real filename + extension
   *  survive; without it a `pasted-<ts>.<ext>` name is synthesized for clipboard bytes. */
  registerStickerSource: (
    owner: string,
    account: string,
    name: string,
    bytes: number[],
  ) => invoke<void>("register_sticker_source", { owner, account, name, bytes }),

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
  searchRecent: (owner: string, query: string) =>
    invoke<RecentSearch>("owner_search_recent", { owner, query }),
  searchContext: (owner: string, reference: SearchReference) =>
    invoke<SearchContext>("owner_search_context", { owner, reference }),
  search: (query: string) => invoke<SearchHitInfo[]>("search", { query }),
  startLinking: (syncAll = false) =>
    invokeSensitive<string>("start_linking", { syncAll }),
  stopLinking: () => invoke<void>("stop_linking"),
  linkDevice: (peer: string, code: string, syncAll = false) =>
    invokeSensitive<string>("link_device", { peer, code, syncAll }),
  rekeyAccount: () => invoke<string>("rekey_account"),
};
