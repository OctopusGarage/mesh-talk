import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  SavedSessionStatus,
  UserInfo,
  CallSignalEvent,
  NodeStateChangedEvent,
  ChannelMessageEvent,
  ChannelChangedEvent,
  DmReceivedEvent,
  FileProgressEvent,
  FileReceivedEvent,
  ProfileReceivedEvent,
  IntegrationMessageAcceptedEvent,
} from "./types";

/** Subscribe to the node's inbound events. Returns a single unlisten function. */
export function subscribeNodeEvents(handlers: {
  onIntegrationMessageAccepted?: (e: IntegrationMessageAcceptedEvent) => void;
  onStateChanged?: (e: NodeStateChangedEvent) => void;
  onDm?: (e: DmReceivedEvent) => void;
  onChannelChanged?: (e: ChannelChangedEvent) => void;
  onChannelMessage?: (e: ChannelMessageEvent) => void;
  onFile?: (e: FileReceivedEvent) => void;
  onFileProgress?: (e: FileProgressEvent) => void;
  onAccountProfile?: (event: {
    user: UserInfo;
    account_id: string;
    device_id: string;
    saved_session?: SavedSessionStatus | null;
  }) => void;
  onProfile?: (e: ProfileReceivedEvent) => void;
  onCallSignal?: (e: CallSignalEvent) => void;
}): () => void {
  const unlisteners: Promise<UnlistenFn>[] = [];
  if (handlers.onIntegrationMessageAccepted) {
    unlisteners.push(
      listen<IntegrationMessageAcceptedEvent>(
        "integration-message-accepted",
        (e) => handlers.onIntegrationMessageAccepted!(e.payload),
      ),
    );
  }
  if (handlers.onStateChanged) {
    unlisteners.push(
      listen<NodeStateChangedEvent>("node-state-changed", (e) =>
        handlers.onStateChanged!(e.payload),
      ),
    );
  }
  if (handlers.onDm) {
    unlisteners.push(
      listen<DmReceivedEvent>("dm-received", (e) => handlers.onDm!(e.payload)),
    );
  }
  if (handlers.onChannelChanged) {
    unlisteners.push(
      listen<ChannelChangedEvent>("channel-changed", (e) =>
        handlers.onChannelChanged!(e.payload),
      ),
    );
  }
  if (handlers.onChannelMessage) {
    unlisteners.push(
      listen<ChannelMessageEvent>("channel-message", (e) =>
        handlers.onChannelMessage!(e.payload),
      ),
    );
  }
  if (handlers.onFile) {
    unlisteners.push(
      listen<FileReceivedEvent>("file-received", (e) =>
        handlers.onFile!(e.payload),
      ),
    );
  }
  if (handlers.onFileProgress) {
    unlisteners.push(
      listen<FileProgressEvent>("file-progress", (e) =>
        handlers.onFileProgress!(e.payload),
      ),
    );
  }
  if (handlers.onAccountProfile) {
    unlisteners.push(
      listen<{
        user: UserInfo;
        account_id: string;
        device_id: string;
        saved_session?: SavedSessionStatus | null;
      }>("account-profile-changed", (e) =>
        handlers.onAccountProfile!(e.payload),
      ),
    );
  }
  if (handlers.onProfile) {
    unlisteners.push(
      listen<ProfileReceivedEvent>("profile-received", (e) =>
        handlers.onProfile!(e.payload),
      ),
    );
  }
  if (handlers.onCallSignal) {
    unlisteners.push(
      listen<CallSignalEvent>("call-signal", (e) =>
        handlers.onCallSignal!(e.payload),
      ),
    );
  }
  return () => {
    for (const u of unlisteners) u.then((fn) => fn()).catch(() => {});
  };
}
