import { startRecoveryRefresh } from "./recoveryRefresh";
import { chat } from "./api/chat";
import { stickerById, stickerPackId } from "./stickerPacks";
import { decodeStickerMedia, snapshotSticker } from "./stickerMedia";
import { errorKind } from "./error";
import {
  subscribeSyncInvalidation,
  isDocumentInvalidation,
} from "./syncInvalidation";
import { usePacks } from "@/store/packs";
import {
  captureRuntimeOwner,
  currentRuntimeAccountId,
} from "@/store/ownership";

/** Supply only native pending transfers. Native hashes remain the reuse authority. */
export function startInstalledStickerSources(owner: string): () => void {
  const lease = captureRuntimeOwner();
  const account = currentRuntimeAccountId();
  let active = true;
  let running = false;
  let dirty = false;
  // Invalid artwork is parked without retaining bytes. Replacement tokens retry.
  const rejected = new Set<string>();
  const park = (key: string) => {
    rejected.add(key);
    if (rejected.size > 128) rejected.delete(rejected.values().next().value!);
  };
  const current = () =>
    active &&
    lease.owner === owner &&
    lease.current() &&
    Boolean(account) &&
    currentRuntimeAccountId() === account;
  const ready = () => {
    const state = usePacks.getState();
    return (
      current() &&
      state.loaded &&
      state.scope?.owner === owner &&
      state.scope.account === account
    );
  };
  const signature = () => {
    const state = usePacks.getState();
    return JSON.stringify([
      state.loaded,
      state.scope,
      state.packs
        .filter((p) => p.kind === "sticker")
        .map((p) => [p, state.contentToken(p.id)]),
    ]);
  };
  let lastSignature = signature();
  const reconcile = async () => {
    if (!ready()) return;
    if (running) {
      dirty = true;
      return;
    }
    running = true;
    let deferredDirty = false;
    try {
      // Finish each suffix before honoring a dirty head: an event during every
      // registration must not starve later rows. Each row is attempted once/sweep.
      let sweeps = 0;
      do {
        sweeps++;
        dirty = false;
        let cursor: string | null = null;
        do {
          const page = await chat.pendingStickerSources(
            owner,
            account!,
            cursor,
            32,
          );
          if (!ready()) return;
          if (
            !page ||
            !Array.isArray(page.items) ||
            page.items.length > 64 ||
            !(
              page.next_cursor === null ||
              (typeof page.next_cursor === "string" &&
                /^[0-9a-f]{64}$/.test(page.next_cursor) &&
                (cursor === null || page.next_cursor > cursor))
            )
          )
            return;
          let previous = cursor;
          for (const item of page.items) {
            if (
              !item ||
              typeof item.file_conv !== "string" ||
              !/^[0-9a-f]{64}$/.test(item.file_conv) ||
              (previous !== null && item.file_conv <= previous) ||
              (page.next_cursor !== null &&
                item.file_conv > page.next_cursor) ||
              typeof item.name !== "string" ||
              typeof item.mime !== "string"
            )
              return;
            previous = item.file_conv;
          }
          for (const item of page.items) {
            if (!ready()) return;
            const identity = decodeStickerMedia({ ...item, media: true });
            if (!identity) continue;
            const state = usePacks.getState();
            const id = stickerPackId(identity.id, state.packs);
            if (!id || !stickerById(identity.id, state.packs)) continue;
            const token = state.contentToken(id);
            const key = JSON.stringify([id, token, item]);
            if (rejected.has(key)) continue;
            const valid = () =>
              ready() && usePacks.getState().contentToken(id) === token;
            try {
              const pack = await state.readPack(id, valid);
              if (!valid()) return;
              if (!pack) continue;
              const sticker = stickerById(identity.id, [pack]);
              if (!sticker) continue;
              let snapshot;
              try {
                snapshot = snapshotSticker(sticker, sticker.emoji);
              } catch {
                park(key);
                continue;
              }
              if (!valid()) return;
              try {
                await chat.registerStickerSource(
                  owner,
                  account!,
                  snapshot.name,
                  Array.from(snapshot.bytes),
                );
              } catch (error) {
                if (
                  valid() &&
                  ["invalid-input", "validation"].includes(
                    errorKind(error) ?? "",
                  )
                )
                  park(key);
              }
              if (!valid()) return;
            } catch {
              /* Storage and IPC failures retry only on a later wake. */
            }
          }
          cursor = page.next_cursor;
          // Yield to the event loop between bounded pages, without retaining payloads.
          if (cursor !== null)
            await new Promise<void>((resolve) => setTimeout(resolve, 0));
        } while (cursor !== null && ready());
      } while (dirty && sweeps < 2 && ready());
      // Registration may itself emit files while a checksum-mismatching local
      // candidate remains pending. Bound self-wakes to one fresh-head retry;
      // retain later dirty work for the next meaningful event or sparse repair.
      deferredDirty = dirty;
    } catch {
      /* A failed demand query waits for an event or repair. */
    } finally {
      running = false;
      if (dirty && !deferredDirty && ready()) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unsubscribe = usePacks.subscribe(() => {
    const next = signature();
    if (next === lastSignature) return;
    lastSignature = next;
    void reconcile();
  });
  const unsubscribeSync = subscribeSyncInvalidation((change) => {
    if (
      change.owner === owner &&
      (change.kind === "files" || isDocumentInvalidation(change))
    )
      void reconcile();
  });
  const stopRecovery = startRecoveryRefresh(() => void reconcile());
  void reconcile();
  return () => {
    active = false;
    unsubscribe();
    unsubscribeSync();
    stopRecovery();
  };
}
