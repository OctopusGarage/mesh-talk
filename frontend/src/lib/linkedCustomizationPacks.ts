import {
  accountDocuments,
  documentVersion,
  sameDocumentVersion,
  type AccountDocumentKey,
  type DocumentVersion,
} from "./api/accountDocuments";
import { chat } from "./api/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import { useChat } from "@/store/chat";
import { usePacks, type StoredPack } from "@/store/packs";
import { archivePack } from "./packArchive";
import { parsePack } from "./pack";
import { verifyPackImages } from "./packImages";
import { subscribeSyncInvalidation } from "./syncInvalidation";
import { readWithDeadline } from "./readDeadline";
const keyFor = (id: string): AccountDocumentKey => ({
  kind: "customization_pack",
  pack_id: id,
});
function scopeFor(owner: string) {
  const lease = captureRuntimeOwner();
  const account = useChat.getState().myAccountId;
  const current = () =>
    lease.owner === owner &&
    lease.current() &&
    Boolean(account) &&
    useChat.getState().myAccountId === account;
  const requireCurrent = () => {
    if (!current())
      throw new Error("Account changed during library synchronization");
  };
  return {
    account: account ?? "",
    session: `${lease.generation}:${lease.run}`,
    current,
    requireCurrent,
  };
}
async function publish(
  owner: string,
  account: string,
  change: StoredPack,
  current: () => boolean,
) {
  const key = keyFor(change.id);
  const meta = await accountDocuments.meta(owner, account, key);
  if (!current()) return;
  const result = change.archive
    ? await accountDocuments.put(
        owner,
        account,
        key,
        change.archive,
        meta ? documentVersion(meta) : null,
      )
    : await accountDocuments.remove(
        owner,
        account,
        key,
        meta ? documentVersion(meta) : null,
      );
  if (current())
    await usePacks
      .getState()
      .acknowledge(change.id, change.sequence, documentVersion(result));
}
/** Only the explicit keep-source action may attribute legacy global libraries. */
export async function publishCustomizationPacksBeforeLink(
  owner: string,
): Promise<void> {
  const scope = scopeFor(owner);
  scope.requireCurrent();
  const policy = await chat.getDeviceSyncPolicy(owner);
  scope.requireCurrent();
  if (!policy.appearance) return;
  const store = usePacks.getState();
  store.setScope(owner, scope.account, scope.session);
  await store.load();
  scope.requireCurrent();
  for (const change of await store.pendingChanges()) {
    scope.requireCurrent();
    await publish(owner, scope.account, change, scope.current);
  }
  for (const pack of await store.legacyPacks()) {
    scope.requireCurrent();
    const key = keyFor(pack.id);
    const existing = await accountDocuments.meta(owner, scope.account, key);
    scope.requireCurrent();
    if (existing) continue; // Existing values and tombstones always retain authority.
    const bytes = archivePack(pack);
    await verifyPackImages(parsePack(bytes));
    scope.requireCurrent();
    let created;
    try {
      created = await accountDocuments.put(
        owner,
        scope.account,
        key,
        bytes,
        null,
      );
    } catch (error) {
      scope.requireCurrent();
      // A sibling may have published after the absent snapshot. Preserve that
      // record; a storage/transport failure with no canonical row remains an error.
      if (!(await accountDocuments.meta(owner, scope.account, key)))
        throw error;
      continue;
    }
    scope.requireCurrent();
    await store.applyLinked(
      pack.id,
      bytes,
      documentVersion(created),
      scope.current,
    );
  }
  scope.requireCurrent();
}
export function startLinkedCustomizationPacks(
  owner: string,
  selectedThemeId?: string,
): () => void {
  const scope = scopeFor(owner);
  let active = true;
  let running = false;
  let dirty = false;
  const current = () => active && scope.current();
  const seen = new Map<string, DocumentVersion>();
  const store = usePacks.getState();
  store.setScope(owner, scope.account, scope.session);
  const installedScope = usePacks.getState().scope;
  const reconcile = async () => {
    if (!current()) return;
    if (running) {
      dirty = true;
      return;
    }
    running = true;
    try {
      await store.load(selectedThemeId);
      if (!current()) return;
      const policy = await readWithDeadline(() =>
        chat.getDeviceSyncPolicy(owner),
      );
      if (!current() || !policy.appearance) return;
      for (const change of await store.pendingChanges()) {
        if (!current()) return;
        try {
          await publish(owner, scope.account, change, current);
        } catch (error) {
          console.warn("Could not publish linked library; will retry", error);
        }
      }
      let after: AccountDocumentKey | null = null;
      for (let page = 0; page < 64; page++) {
        const result = await readWithDeadline(() =>
          accountDocuments.list(
            owner,
            scope.account,
            "customization_pack",
            after,
          ),
        );
        if (!current()) return;
        for (const meta of result.items) {
          if (meta.key.kind !== "customization_pack") continue;
          const id = meta.key.pack_id;
          const version = documentVersion(meta);
          if (seen.has(id) && sameDocumentVersion(seen.get(id)!, version))
            continue;
          const bytes = meta.tombstone
            ? null
            : new Uint8Array(
                await readWithDeadline(() =>
                  accountDocuments.read(
                    owner,
                    scope.account,
                    meta.key,
                    version,
                  ),
                ),
              );
          if (!current()) return;
          if (await store.applyLinked(id, bytes, version, current))
            seen.set(id, version);
        }
        after = result.next;
        if (!after) break;
      }
    } catch (error) {
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked libraries", error);
    } finally {
      running = false;
      if (current() && dirty) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unsubscribe = usePacks.subscribe(() => void reconcile());
  const unlisten = subscribeSyncInvalidation((change) => {
    if (change.owner === owner && change.kind === "state") void reconcile();
  });
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    unsubscribe();
    unlisten();
    window.clearInterval(timer);
    if (usePacks.getState().scope === installedScope)
      store.setScope(null, null);
  };
}
