import { invoke } from "@tauri-apps/api/core";
import type { FavoriteInfo, HiddenContactsSnapshot } from "../types";

export const contactPolicy = {
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
  get: () => invoke<FavoriteInfo[]>("get_favorites"),
  /** Pin or unpin a contact by id. */
  setFavorite: (id: string, pinned: boolean) =>
    invoke<void>("set_favorite", { id, pinned }),
  /** Set or clear (null/blank) a contact's custom alias. */
  setAlias: (id: string, alias: string | null) =>
    invoke<void>("set_alias", { id, alias }),
};

export const avatars = {
  /** Every custom avatar the user has set LOCALLY, as a map of `id -> data-URL`. */
  get: () => invoke<Record<string, string>>("get_avatars"),
  /** Set (data-URL) or clear (null) a LOCAL custom avatar for an identity by id. */
  set: (id: string, dataUrl: string | null) =>
    invoke<void>("set_avatar", { id, dataUrl }),
  /**
   * Every avatar peers PROPAGATED to us, as `account_id -> data-URL`. Merged under local
   * overrides so a received avatar survives a relaunch (the node persists it).
   */
  peers: () => invoke<Record<string, string>>("peer_avatars"),
  /**
   * Publish (or clear with null) THIS user's own avatar to peers as a signed profile.
   * Call when the user sets/removes their own photo; contacts then render it.
   */
  publish: (dataUrl: string | null) =>
    invoke<void>("publish_avatar", { avatar: dataUrl }),
};
