import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  CallSignalEvent,
  ChannelMessageEvent,
  DmReceivedEvent,
  FileProgressEvent,
  FileReceivedEvent,
  ProfileReceivedEvent,
} from "./types";

/** Subscribe to the node's inbound events. Returns a single unlisten function. */
export function subscribeNodeEvents(handlers: {
  onDm?: (e: DmReceivedEvent) => void;
  onChannelMessage?: (e: ChannelMessageEvent) => void;
  onFile?: (e: FileReceivedEvent) => void;
  onFileProgress?: (e: FileProgressEvent) => void;
  onProfile?: (e: ProfileReceivedEvent) => void;
  onCallSignal?: (e: CallSignalEvent) => void;
}): () => void {
  const unlisteners: Promise<UnlistenFn>[] = [];
  if (handlers.onDm) {
    unlisteners.push(
      listen<DmReceivedEvent>("dm-received", (e) => handlers.onDm!(e.payload)),
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
