import { chat } from "./api/chat";
import { installedStickers } from "./stickerPacks";
import { snapshotSticker } from "./stickerMedia";
import { usePacks } from "@/store/packs";
import {
  captureRuntimeOwner,
  currentRuntimeAccountId,
} from "@/store/ownership";

/** Make installed artwork available locally before an incoming transfer needs it. */
export function startInstalledStickerSources(owner: string): () => void {
  const lease = captureRuntimeOwner();
  const account = currentRuntimeAccountId();
  let active = true;
  let running = false;
  let dirty = false;
  const registered = new Map<string, string>();
  const current = () =>
    active &&
    lease.owner === owner &&
    lease.current() &&
    Boolean(account) &&
    currentRuntimeAccountId() === account;
  const reconcile = async () => {
    if (!current()) return;
    if (running) {
      dirty = true;
      return;
    }
    const state = usePacks.getState();
    if (
      !state.loaded ||
      state.scope?.owner !== owner ||
      state.scope.account !== account
    )
      return;
    running = true;
    try {
      const stickers = installedStickers(state.packs);
      const installed = new Set(
        stickers.map((sticker) => `${sticker.id}\n${sticker.emoji}`),
      );
      for (const identity of registered.keys())
        if (!installed.has(identity)) registered.delete(identity);
      for (const sticker of stickers) {
        if (!current()) return;
        const identity = `${sticker.id}\n${sticker.emoji}`;
        if (registered.get(identity) === sticker.url) continue;
        try {
          const snapshot = snapshotSticker(sticker, sticker.emoji);
          await chat.registerStickerSource(
            owner,
            account!,
            snapshot.name,
            Array.from(snapshot.bytes),
          );
          if (!current()) return;
          registered.set(identity, sticker.url);
        } catch {
          // This optional local cache never blocks pack installation or normal transfer.
          // Transient startup/lock failures are retried while this runtime remains current.
        }
      }
    } finally {
      running = false;
      if (dirty && current()) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unsubscribe = usePacks.subscribe(() => void reconcile());
  const timer = window.setInterval(() => void reconcile(), 10_000);
  void reconcile();
  return () => {
    active = false;
    unsubscribe();
    window.clearInterval(timer);
  };
}
