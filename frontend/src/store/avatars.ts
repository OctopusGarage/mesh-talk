import { create } from "zustand";
import { avatars as avatarsApi } from "@/lib/api";

/**
 * Custom avatars slice. A user can set a profile photo for themselves or any contact,
 * replacing the deterministic IdentityGlyph wherever that identity renders.
 *
 * Two sources, by id (account_id / channel_id):
 * - `local`: photos the user set on THIS device (persisted as `avatars.json`). These are
 *   local overrides — a contact photo you chose for someone, or your own photo.
 * - `received`: avatars peers PROPAGATED to us via signed profiles (persisted by the node,
 *   keyed by account id). These let `useAvatar(account_id)` show a contact's chosen photo.
 *
 * PRECEDENCE: a `local` override always wins over a `received` one (if you set a custom
 * photo for a contact, that's what you see). Your own local avatar is unchanged by what
 * peers send. When you set/clear your OWN avatar it is also PUBLISHED to peers.
 */

interface AvatarsState {
  /** Locally-set overrides: id -> data-URL. */
  local: Record<string, string>;
  /** Received peer avatars: account_id -> data-URL. */
  received: Record<string, string>;
  /** This user's own account id, so `setAvatar` knows when to publish to peers. */
  ownId: string;
  /** Tell the store which id is "us" (set once the node resolves the account id). */
  setOwnId: (id: string) => void;
  /** Load the persisted LOCAL avatar table from the backend (called on app start). */
  load: () => Promise<void>;
  /** Load received peer avatars from the node (called once the node is ready). */
  loadPeers: () => Promise<void>;
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
  reassertOwn: () => Promise<void>;
}

/** Stable empty map so a "no avatars yet" load keeps a constant ref. */
const EMPTY: Record<string, string> = {};

export const useAvatars = create<AvatarsState>((set, get) => ({
  local: EMPTY,
  received: EMPTY,
  ownId: "",

  setOwnId: (id) => set({ ownId: id }),

  load: async () => {
    try {
      set({ local: (await avatarsApi.get()) ?? EMPTY });
    } catch {
      // avatars are local UI personalization; a load failure is non-fatal.
    }
  },

  loadPeers: async () => {
    try {
      set({ received: (await avatarsApi.peers()) ?? EMPTY });
    } catch {
      // the node may not be ready yet; the live event still fills `received`.
    }
  },

  mergeReceived: (id, dataUrl) =>
    set((s) => {
      const next = { ...s.received };
      if (dataUrl) next[id] = dataUrl;
      else delete next[id];
      return { received: next };
    }),

  setAvatar: async (id, dataUrl) => {
    // Optimistic: update the local mirror immediately so every identity spot re-renders.
    set((s) => {
      const next = { ...s.local };
      if (dataUrl) next[id] = dataUrl;
      else delete next[id];
      return { local: next };
    });
    try {
      await avatarsApi.set(id, dataUrl);
      // Setting/clearing OUR OWN avatar propagates it to peers (signed profile).
      if (id === get().ownId) await avatarsApi.publish(dataUrl);
    } catch {
      // On failure, reconcile the local mirror from disk so it matches what was stored.
      await get().load();
    }
  },

  reassertOwn: async () => {
    // Ensure the persisted local table is loaded, then re-publish our own avatar (if any)
    // so the node holds it again and propagates it to peers (including ones we discover
    // later this session).
    await get().load();
    const { ownId, local } = get();
    const mine = ownId ? local[ownId] : undefined;
    if (mine) {
      try {
        await avatarsApi.publish(mine);
      } catch {
        // best-effort: a peer will still get it next time we change the avatar.
      }
    }
  },
}));

/**
 * Subscribe to one identity's avatar by id: a LOCAL override wins, else the avatar a peer
 * propagated to us. Returns a stable string|undefined so a component only re-renders when
 * this id's photo actually changes.
 */
export function useAvatar(id: string | undefined): string | undefined {
  return useAvatars((s) => (id ? (s.local[id] ?? s.received[id]) : undefined));
}
