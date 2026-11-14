import { startRecoveryRefresh } from "./recoveryRefresh";
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
  const registered = new Map<
    string,
    { token: string; completed: Set<string> }
  >();
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
      const libraries = state.packs.filter((pack) => pack.kind === "sticker");
      const installed = new Set(libraries.map((pack) => pack.id));
      for (const id of registered.keys())
        if (!installed.has(id)) registered.delete(id);
      // Only one transient pack is held at a time. The UI catalog and successful
      // receipts never own artwork; native reuse retains its existing 128/32 MiB cap.
      for (const library of libraries) {
        if (!current()) return;
        const token = state.contentToken(library.id);
        let receipt = registered.get(library.id);
        if (receipt?.token !== token) {
          receipt = { token, completed: new Set() };
          registered.set(library.id, receipt);
        }
        if (
          library.kind !== "sticker" ||
          receipt.completed.size === library.stickers.length
        )
          continue;
        try {
          const pack = await state.readPack(library.id, current);
          if (
            !current() ||
            usePacks.getState().contentToken(library.id) !== token ||
            !pack
          )
            continue;
          for (const sticker of installedStickers([pack])) {
            if (
              !current() ||
              usePacks.getState().contentToken(library.id) !== token
            )
              break;
            if (receipt.completed.has(sticker.id)) continue;
            try {
              const snapshot = snapshotSticker(sticker, sticker.emoji);
              await chat.registerStickerSource(
                owner,
                account!,
                snapshot.name,
                Array.from(snapshot.bytes),
              );
              if (
                !current() ||
                usePacks.getState().contentToken(library.id) !== token
              )
                break;
              receipt.completed.add(sticker.id);
            } catch {
              /* Failed candidates retry on the existing recovery wake. */
            }
          }
        } catch {
          /* Transient storage reads remain retryable. */
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
  const stopRecovery = startRecoveryRefresh(() => void reconcile());
  void reconcile();
  return () => {
    active = false;
    unsubscribe();
    stopRecovery();
  };
}
