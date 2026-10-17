// TypeScript mirrors of the Rust IPC structs (serde serializes field names as-is,
// so these are snake_case). Keep in sync with src-tauri/src/chat_commands.rs + events.rs.

export interface UserInfo {
  id: string;
  /** The immutable login handle. */
  username: string;
  /** The editable, peer-facing display name (nickname). Equals `username` until changed. */
  display_name: string;
}

export interface LoginResult {
  success: boolean;
  token?: string;
  user?: UserInfo;
}

export interface RegisterResult {
  success: boolean;
  user?: UserInfo;
}

export interface LogoutResult {
  success: boolean;
}

export interface PeerInfo {
  user_id: string;
  name: string;
  addr: string;
  post_office: boolean;
  /** Directly discovered LAN route; absent only with an older desktop backend. */
  discovered_on_lan?: boolean;
  /** Known direct application route; transport rechecks connectivity and identity. */
  directly_reachable?: boolean;
  account_id: string | null;
}

export interface HistoryFileInfo {
  file_conv: string; // hex — pass to read_file/save_file
  name: string;
  size: number;
  mime: string;
  media: boolean; // inline media (media button) vs attachment (attach button), by intent
}

export interface HistoryItem {
  id: string | null; // null when there is no stable event id yet
  from_me: boolean;
  who: string;
  text: string;
  wall_clock: number;
  reply_to: string | null;
  file: HistoryFileInfo | null; // present when this line is a file/media message
  recalled: boolean; // true when recalled → render a placeholder, no content
  recalled_text: string | null; // our own recalled text, for "re-edit" (null otherwise)
  sticker: string | null; // animated-sticker id when this message is a sticker
}

export interface ReactionInfo {
  target: string;
  emoji: string;
  who: string[];
}

export interface ChannelMemberInfo {
  user_id: string;
  name: string;
  account_id?: string | null;
  pending?: boolean;
  replica_enabled?: boolean;
}

export interface ChannelActivityInfo {
  kind:
    | "created"
    | "joined"
    | "removed"
    | "renamed"
    | "transferred"
    | "left"
    | "accepted";
  actor: string;
  target: string | null;
  wall_clock: number;
}

// A channel's membership plus its owner. The owner is the only principal allowed to
// change membership (enforced in core) — the UI shows the owner badge and reveals the
// add/remove controls only to the owner.
export interface ChannelMembersInfo {
  owner: string;
  members: ChannelMemberInfo[];
}

export interface AccountInfo {
  account_id: string;
  device_count: number;
  names: string[];
}

export interface HiddenContact {
  account_id: string;
  name: string;
}

export interface HiddenContactsSnapshot {
  /** Device-local contact deletions, cleared by fresh online discovery. */
  deleted?: string[];
  owner: string;
  contacts: HiddenContact[];
}

export interface ChannelInfo {
  channel_id: string;
  name: string;
  member_count: number;
  // The owner's device user_id. Only the owner may rename the channel (synced to all
  // members); non-owners fall back to a local-only alias.
  owner: string;
  archived?: boolean;
  dissolved?: boolean;
  left?: boolean;
  pending_invite?: boolean;
}

export interface SearchHitInfo {
  is_channel: boolean;
  /** Account binding at search time, retained even if the live device leaves. */
  account_id?: string | null;
  target: string;
  label: string;
  from_me: boolean;
  who: string;
  text: string;
  wall_clock: number;
}

// --- Diagnostics / discovery (src-tauri/src/chat_commands.rs) ---

export interface DiagPeerInfo {
  user_id: string;
  name: string;
  ip: string;
  tcp_port: number;
  via_gateway?: boolean;
  post_office: boolean;
  account_id: string | null;
  last_seen_secs: number;
}

export interface DiagNetworkInfo {
  own_user_id: string;
  own_name: string;
  account_id: string;
  listen_tcp_port: number;
  discovery_port: number;
  multicast_group: string;
  interfaces: string[];
}

// --- Presence (online / last-seen) (src-tauri/src/chat_commands.rs) ---

export interface PresenceEvidence {
  source:
    "local" | "routed" | "directory" | "authenticated" | "unscoped" | "self";
  last_seen_secs: number;
  expires_in_secs: number;
}
export interface PresenceDevice {
  user_id: string;
  evidence: PresenceEvidence[];
}
export interface PresenceInfo {
  /** Person has fresh eligible device evidence; groups use member counts instead. */
  online: boolean;
  devices?: PresenceDevice[];
  member_count?: number;
  online_count?: number;
  unbound_member_count?: number;
  member_keys?: string[];
  /** Whole seconds since the most-recently-seen relevant device (null = never seen). */
  last_seen_secs: number | null;
}

/** Accounts, explicit device:<id> legacy identities, and channel count summaries. */
export type PresenceMap = Record<string, PresenceInfo>;

// --- Background-presence settings (src-tauri/src/settings.rs) ---

export interface AppSettings {
  /** Closing the window hides it to the tray instead of quitting. */
  minimize_to_tray: boolean;
  /** Native notification on incoming messages when the window isn't focused. */
  notifications: boolean;
  /** Remembered default folder received files save into ("" = always prompt). */
  download_dir: string;
  /** "Stay signed in": persist the password in the OS keychain to auto-unlock next launch. */
  stay_signed_in: boolean;
  /** The username to auto-login next launch (backend-managed; not set from the UI). */
  last_user: string | null;
  /** Keep chat history for at most this many days (0 = forever); older messages are erased. */
  retention_days: number;
  /** Voice/video calls (experimental, 1:1, LAN-only). Default off; opt-in via Settings. */
  calls_enabled: boolean;
  /** Use an available LAN post-office relay for offline delivery. */
  post_office_enabled: boolean;
  post_office_send_enabled: boolean;
  post_office_receive_enabled: boolean;
  physical_lan_only: boolean;
  /** Share public presence through an opt-in post-office bridge. */
  bridge_discovery_enabled: boolean;
  /** Preferred directly reachable post-office fingerprint; empty selects automatically with eligible alternatives. */
  bridge_hub_id: string;
  /** Incoming-call ringtone id (see lib/ringtones.ts). Defaults to "classic". */
  ringtone: string;
  /** Preset name; custom means individual privacy controls differ from a preset. */
  privacy_level: "balanced" | "private" | "strict" | "custom";
  notification_preview: boolean;
  auto_lock_seconds: number;
  lock_when_hidden: boolean;
  confirm_sensitive_copy: boolean;
  clipboard_clear_seconds: number;
}

// --- Environment / About (src-tauri/src/diagnostics.rs) ---

export interface EnvInfo {
  app_version: string;
  data_dir: string;
  logs_dir: string;
  os: string;
  arch: string;
  target: string;
  build_profile: string;
}

export interface UpdateCheck {
  current_version: string;
  latest_version: string;
  is_newer: boolean;
  release_url: string;
  notes: string;
  package_name: string | null;
  package_size: number | null;
  edition: "default" | "lite";
}

export interface DownloadProgress {
  downloaded: number;
  total: number;
}

export interface DownloadedUpdate {
  path: string;
  file_name: string;
}

// --- Contact trust / safety numbers (src-tauri/src/trust.rs + chat_commands.rs) ---

export interface TrustInfo {
  account_id: string;
  /** The fingerprint pinned on first contact (the one to verify out-of-band). */
  first_seen_fingerprint: string;
  verified: boolean;
  /** A known contact's fingerprint changed since first contact — warn loudly. */
  fingerprint_changed: boolean;
  /** False = brand-new contact with no record yet. */
  known: boolean;
}

export interface SafetyNumber {
  grouped: string;
  words: string[];
}

// --- Favorites / pinned contacts (src-tauri/src/favorites.rs) ---

export interface FavoriteInfo {
  /** account_id (or channel_id) the preference is keyed by. */
  id: string;
  /** Pinned contacts sort to the top of the sidebar. */
  pinned: boolean;
  /** A user-set alias that overrides the announced name (null = use announced name). */
  custom_alias: string | null;
  hidden?: boolean;
}

// --- Tauri events (src-tauri/src/events.rs) ---

export interface DmReceivedEvent {
  from: string;
  from_name: string;
  text: string;
  reply_to: string | null;
}

export interface ChannelMessageEvent {
  channel_id: string;
  channel_name: string;
  from: string;
  text: string;
  reply_to: string | null;
}

export interface FileReceivedEvent {
  conv: string;
  is_channel: boolean; // emitted by the same installed native app, not over the network
  from: string;
  name: string;
  size: number;
  mime: string;
  file_conv: string;
  media: boolean; // inline media (media button) vs attachment (attach button), by intent
}

export interface FileProgressEvent {
  file_conv: string; // per-file id for saves, optimistic message key for sends
  direction: "prepare" | "send" | "save";
  done: number;
  total: number;
}

export interface OutgoingFileStatus {
  done: number;
  total: number;
  delivered: boolean;
  consent?: "pending" | "accepted" | "declined";
}

export interface FileStatus {
  file_conv: string;
  done: number;
  total: number;
  ready: boolean;
  consent?: "pending" | "accepted" | "declined" | null;
}

export interface AttachmentReceivePolicy {
  global_auto: boolean;
  contact_auto: boolean | null;
}

export interface ProfileReceivedEvent {
  account_id: string;
  avatar: string | null; // data-URL, or null when the peer cleared their avatar
}

export interface CallSignalEvent {
  from: string; // authenticated sender device user_id (bound to the Noise channel)
  payload: string; // opaque signaling JSON (call id, kind, SDP) — see store/calls.ts
}
export interface PrivacySnapshot {
  owner: string;
  version: number;
  invisible: boolean;
  allowed_accounts: {
    id: string;
    name: string;
    source: "Manual" | "Initiated";
  }[];
}
