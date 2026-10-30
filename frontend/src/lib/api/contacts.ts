import { invoke } from "@tauri-apps/api/core";
import type { FavoriteInfo, HiddenContactsSnapshot } from "../types";

export const contactPolicy = {
  apply: (
    owner: string,
    expectedAccount: string,
    records: Array<{ account: string; hidden: boolean; name: string }>,
    expected: Record<string, string | null>,
  ) =>
    invoke<HiddenContactsSnapshot>("set_contact_hidden_batch", {
      owner,
      expectedAccount,
      records,
      expected,
    }),
  delete: (
    owner: string,
    expectedAccount: string,
    account: string,
    clearHistory = true,
  ) =>
    invoke<HiddenContactsSnapshot>("delete_contact", {
      owner,
      expectedAccount,
      account,
      clearHistory,
    }),
  get: (owner: string) =>
    invoke<HiddenContactsSnapshot>("get_hidden_contacts", { owner }),
  set: (
    owner: string,
    account: string,
    hidden: boolean,
    name: string,
    expectedAccount?: string,
  ) =>
    invoke<HiddenContactsSnapshot>("set_contact_hidden", {
      owner,
      account,
      hidden,
      name,
      ...(expectedAccount === undefined ? {} : { expectedAccount }),
    }),
};

export const favorites = {
  apply: (
    owner: string,
    expectedAccount: string,
    records: FavoriteInfo[],
    expected: Record<string, Omit<FavoriteInfo, "id"> | null>,
  ) =>
    invoke<void>("set_favorite_records", {
      owner,
      expectedAccount,
      records,
      expected,
    }),
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
  get: (owner: string, expectedAccount?: string) =>
    invoke<Record<string, string>>("get_avatars", {
      owner,
      ...(expectedAccount ? { expectedAccount } : {}),
    }),
  /** Set (data-URL) or clear (null) a LOCAL custom avatar for an identity by id. */
  set: (
    owner: string,
    id: string,
    dataUrl: string | null,
    expectedAccount?: string,
  ) =>
    invoke<void>("set_avatar", {
      owner,
      id,
      dataUrl,
      ...(expectedAccount ? { expectedAccount } : {}),
    }),
  publishLegacy: (owner: string, expectedAccount: string) =>
    invoke<number>("publish_legacy_avatar_overrides", {
      owner,
      expectedAccount,
    }),
  /** Exact current value distinguishes a clear tombstone from no signed profile. */
  peer: (owner: string, accountId: string) =>
    invoke<{ account_id: string; avatar: string | null } | null>(
      "peer_profile_avatar",
      { owner, accountId },
    ),
  /**
   * Publish (or clear with null) THIS user's own avatar to peers as a signed profile.
   * Call when the user sets/removes their own photo; contacts then render it.
   */
  publish: (owner: string, expectedAccount: string, dataUrl: string | null) =>
    invoke<void>("publish_avatar", { owner, expectedAccount, avatar: dataUrl }),
};
