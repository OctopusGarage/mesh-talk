import { invoke } from "@tauri-apps/api/core";
import type { FavoriteInfo, HiddenContactsSnapshot } from "../types";

export const contactPolicy = {
  delete: (owner: string, account: string) =>
    invoke<HiddenContactsSnapshot>("delete_contact", { owner, account }),
  get: (owner: string) =>
    invoke<HiddenContactsSnapshot>("get_hidden_contacts", { owner }),
  set: (owner: string, account: string, hidden: boolean, name: string) =>
    invoke<HiddenContactsSnapshot>("set_contact_hidden", {
      owner,
      account,
      hidden,
      name,
    }),
};

export const favorites = {
  /** Every favorites entry the user has set (pin and/or alias), keyed by id. */
  get: (owner: string) => invoke<FavoriteInfo[]>("get_favorites", { owner }),
  /** Pin or unpin a contact by id. */
  setFavorite: (owner: string, id: string, pinned: boolean) =>
    invoke<void>("set_favorite", { owner, id, pinned }),
  /** Set or clear (null/blank) a contact's custom alias. */
  setAlias: (owner: string, id: string, alias: string | null) =>
    invoke<void>("set_alias", { owner, id, alias }),
  setChannelHidden: (owner: string, id: string, hidden: boolean) =>
    invoke<void>("set_channel_hidden", { owner, id, hidden }),
};

export const avatars = {
  /** Every custom avatar the user has set LOCALLY, as a map of `id -> data-URL`. */
  get: (owner: string) =>
    invoke<Record<string, string>>("get_avatars", { owner }),
  /** Set (data-URL) or clear (null) a LOCAL custom avatar for an identity by id. */
  set: (owner: string, id: string, dataUrl: string | null) =>
    invoke<void>("set_avatar", { owner, id, dataUrl }),
  /**
   * Every signed profile peers propagated, including null clear tombstones. The node
   * persists them so a linked-device clear survives a relaunch.
   */
  peers: () => invoke<Record<string, string | null>>("peer_avatars"),
  /**
   * Publish (or clear with null) THIS user's own avatar to peers as a signed profile.
   * Call when the user sets/removes their own photo; contacts then render it.
   */
  publish: (dataUrl: string | null) =>
    invoke<void>("publish_avatar", { avatar: dataUrl }),
};
