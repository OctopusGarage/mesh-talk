import { create } from "zustand";
import { avatars as avatarsApi } from "@/lib/api";
import { captureRuntimeOwner } from "./ownership";
import { useAuth } from "./auth";

/**
 * Custom avatars slice. A user can set a profile photo for themselves or any contact,
 * replacing the deterministic IdentityGlyph wherever that identity renders.
 *
 * Two sources, by id (account_id / channel_id):
 * - `local`: photos the user set on THIS device (persisted per host account). These are
 *   local overrides — a contact photo you chose for someone, or your own photo.
 * - `received`: avatars peers PROPAGATED to us via signed profiles (persisted by the node,
 *   keyed by account id). These let `useAvatar(account_id)` show a contact's chosen photo.
 *
 * PRECEDENCE: a `local` override wins for contacts. A signed update from a linked
 * device replaces the old local copy of our own avatar.
 */

interface AvatarsState {
  owner: string | null;
  setOwner: (owner: string | null) => void;
  /** Locally-set overrides: id -> data-URL. */
  local: Record<string, string>;
  /** Received peer avatars: account_id -> data-URL. */
  received: Record<string, string>;
  /** A durable signed profile exists for our account, including a clear tombstone. */
  ownProfileSeen: boolean;
  /** This user's own account id, so `setAvatar` knows when to publish to peers. */
  ownId: string;
  /** Tell the store which id is "us" (set once the node resolves the account id). */
  setOwnId: (id: string) => void;
  /** Load the persisted LOCAL avatar table from the backend (called on app start). */
  load: (current?: () => boolean) => Promise<void>;
  /** Load received peer avatars from the node (called once the node is ready). */
  loadPeers: (current?: () => boolean) => Promise<boolean>;
  /** Merge one received peer avatar (from the live `profile-received` event). */
  mergeReceived: (id: string, dataUrl: string | null) => void;
  /**
   * Set (dataUrl) or clear (null) the LOCAL avatar for an id — optimistic + persist. When
   * the id is our own account id, also publish the change to peers as a signed profile.
   */
  setAvatar: (id: string, dataUrl: string | null) => Promise<void>;
  /**
   * Re-publish our persisted own avatar to peers on login. The node only re-broadcasts a
   * profile to freshly-discovered peers if it's holding our current one in memory, and that
   * cache is empty after a restart — so without this, a contact discovered after we relaunch
   * never gets our photo until we manually re-set it. Re-asserting on boot fixes that.
   */
  reassertOwn: (current?: () => boolean) => Promise<void>;
}

/** Stable empty map so a "no avatars yet" load keeps a constant ref. */
const EMPTY: Record<string, string> = {};
let localRevision = 0;
let receivedRevision = 0;
let pendingWrite: Promise<void> = Promise.resolve();
const pendingEdits = new Map<
  string,
  { revision: number; dataUrl: string | null; current: () => boolean }
>();

export const useAvatars = create<AvatarsState>((set, get) => ({
  owner: null,
  setOwner: (owner) => {
    if (get().owner === owner) return;
    localRevision++;
    receivedRevision++;
    pendingEdits.clear();
    set({
      owner,
      local: EMPTY,
      received: EMPTY,
      ownId: "",
      ownProfileSeen: false,
    });
  },
  local: EMPTY,
  received: EMPTY,
  ownProfileSeen: false,
  ownId: "",

  setOwnId: (id) => set({ ownId: id }),

  load: async (current = () => true) => {
    const lease = captureRuntimeOwner();
    const revision = localRevision;
    if (!lease.current() || !current()) return;
    try {
      const local = (await avatarsApi.get(lease.owner!)) ?? EMPTY;
      if (
        lease.current() &&
        current() &&
        get().owner === lease.owner &&
        revision === localRevision
      ) {
        const merged = { ...local };
        for (const [id, edit] of pendingEdits) {
          if (!edit.current()) continue;
          if (edit.dataUrl) merged[id] = edit.dataUrl;
          else delete merged[id];
        }
        if (
          get().ownProfileSeen &&
          get().ownId &&
          !pendingEdits.has(get().ownId)
        )
          delete merged[get().ownId];
        set({ local: merged });
      }
    } catch {
      // avatars are local UI personalization; a load failure is non-fatal.
    }
  },

  loadPeers: async (current = () => true) => {
    const lease = captureRuntimeOwner();
    const revision = localRevision;
    const peerRevision = receivedRevision;
    if (!lease.current() || !current()) return false;
    try {
      const profiles = (await avatarsApi.peers()) ?? EMPTY;
      if (
        lease.current() &&
        current() &&
        get().owner === lease.owner &&
        revision === localRevision &&
        peerRevision === receivedRevision
      ) {
        const received = Object.fromEntries(
          Object.entries(profiles).filter(
            (entry): entry is [string, string] => entry[1] !== null,
          ),
        );
        set({ received });
        const ownId = get().ownId;
        if (ownId && Object.prototype.hasOwnProperty.call(profiles, ownId))
          get().mergeReceived(ownId, profiles[ownId]);
        return true;
      }
    } catch {
      // the node may not be ready yet; the live event still fills `received`.
    }
    return false;
  },

  mergeReceived: (id, dataUrl) => {
    receivedRevision++;
    const lease = captureRuntimeOwner();
    const own =
      id === get().ownId && lease.current() && get().owner === lease.owner;
    const replaceOwn = own && !pendingEdits.has(id);
    set((s) => {
      const next = { ...s.received };
      if (dataUrl) next[id] = dataUrl;
      else delete next[id];
      if (!replaceOwn) return { received: next };
      const local = { ...s.local };
      delete local[id];
      return { received: next, local, ownProfileSeen: true };
    });
    if (!replaceOwn) return;
    const revision = ++localRevision;
    pendingEdits.set(id, { revision, dataUrl: null, current: lease.current });
    const task = pendingWrite.then(async () => {
      if (!lease.current()) return;
      try {
        await avatarsApi.set(lease.owner!, id, null);
      } finally {
        if (pendingEdits.get(id)?.revision === revision)
          pendingEdits.delete(id);
      }
    });
    pendingWrite = task.catch(() => {});
  },

  setAvatar: async (id, dataUrl) => {
    const lease = captureRuntimeOwner();
    const ownId = get().ownId;
    if (!lease.current() || get().owner !== lease.owner) return;
    const revision = ++localRevision;
    pendingEdits.set(id, { revision, dataUrl, current: lease.current });
    if (id === ownId) {
      receivedRevision++;
      set((s) => {
        const received = { ...s.received };
        delete received[id];
        return { received, ownProfileSeen: false };
      });
    }
    // Optimistic: update the local mirror immediately so every identity spot re-renders.
    set((s) => {
      const next = { ...s.local };
      if (dataUrl) next[id] = dataUrl;
      else delete next[id];
      return { local: next };
    });
    const currentEdit = () =>
      lease.current() && pendingEdits.get(id)?.revision === revision;
    // Native writes snapshot the full avatar table. Keep their completion order aligned
    // with the user's edits, including changes to different contacts.
    const task = pendingWrite.then(async () => {
      if (!lease.current()) return;
      try {
        await avatarsApi.set(lease.owner!, id, dataUrl);
        // Setting/clearing OUR OWN avatar propagates it to peers (signed profile).
        if (currentEdit() && id === ownId) await avatarsApi.publish(dataUrl);
      } catch {
        // A superseded failure must not replace a newer optimistic edit.
        if (currentEdit()) {
          pendingEdits.delete(id);
          await get().load(lease.current);
        }
      } finally {
        if (pendingEdits.get(id)?.revision === revision)
          pendingEdits.delete(id);
      }
    });
    pendingWrite = task.catch(() => {});
    await task;
  },

  reassertOwn: async (current = () => true) => {
    const lease = captureRuntimeOwner();
    const ownId = get().ownId;
    const active = () => lease.current() && current();
    if (!active()) return;
    // Ensure the persisted local table is loaded, then re-publish our own avatar (if any)
    // so the node holds it again and propagates it to peers (including ones we discover
    // later this session).
    await get().load(active);
    if (!active()) return;
    const revision = localRevision;
    if (!(await get().loadPeers(active))) return;
    if (!active() || revision !== localRevision || get().ownId !== ownId)
      return;
    const mine =
      ownId && !get().ownProfileSeen ? get().local[ownId] : undefined;
    if (!mine || pendingEdits.has(ownId)) return;
    const task = pendingWrite.then(async () => {
      if (
        active() &&
        revision === localRevision &&
        get().ownId === ownId &&
        !get().ownProfileSeen &&
        !pendingEdits.has(ownId) &&
        get().local[ownId] === mine
      ) {
        try {
          await avatarsApi.publish(mine);
        } catch {
          // A later startup or edit will retry publication.
        }
      }
    });
    pendingWrite = task.catch(() => {});
    await task;
  },
}));

/**
 * Subscribe to one identity's avatar by id: a LOCAL override wins, else the avatar a peer
 * propagated to us. Returns a stable string|undefined so a component only re-renders when
 * this id's photo actually changes.
 */
export function useAvatar(id: string | undefined): string | undefined {
  const owner = useAuth((s) => s.user?.id);
  return useAvatars((s) =>
    id && s.owner === owner ? (s.local[id] ?? s.received[id]) : undefined,
  );
}
