import { invoke } from "@tauri-apps/api/core";

export interface FavoriteSource {
  kind: "account" | "channel";
  target: string;
  message_ids: string[];
}
export interface FavoriteMetadata {
  title: string;
  description: string;
  notes: Record<string, string>;
}
export interface FavoriteMessage {
  id: string;
  who: string;
  sender_name: string;
  text: string;
  wall_clock: number;
  expires_at_ms: number | null;
  sticker: string | null;
  file: {
    file_conv: string;
    name: string;
    size: number;
    mime: string;
    media: boolean;
  } | null;
}
export interface MessageFavorite {
  id: string;
  source: FavoriteSource;
  source_label: string;
  metadata: FavoriteMetadata;
  messages: FavoriteMessage[];
  saved_at_ms: number;
  revision: number;
  device_id: string;
}
export interface MessageFavoriteList {
  items: MessageFavorite[];
  has_more: boolean;
}
export const messageFavorites = {
  inspect: (owner: string, source: FavoriteSource) =>
    invoke<{ existing: MessageFavorite | null; saved_message_ids: string[] }>(
      "owner_inspect_message_favorites",
      { owner, source },
    ),
  fileAvailable: (owner: string, fileConv: string) =>
    invoke<boolean>("owner_favorite_file_available", { owner, fileConv }),
  readFile: async (owner: string, fileConv: string) => {
    const bytes = await invoke<ArrayBuffer | number[]>(
      "owner_read_favorite_file",
      { owner, fileConv },
    );
    return Array.isArray(bytes) ? new Uint8Array(bytes).buffer : bytes;
  },
  saveFile: (
    owner: string,
    fileConv: string,
    dest: string,
    toDirectory: boolean,
  ) =>
    invoke<string>("owner_save_favorite_file", {
      owner,
      fileConv,
      dest,
      toDirectory,
    }),
  list: (owner: string, query: string, offset: number, limit: number) =>
    invoke<MessageFavoriteList>("owner_list_message_favorites", {
      owner,
      query,
      offset,
      limit,
    }),
  save: (owner: string, source: FavoriteSource, metadata: FavoriteMetadata) =>
    invoke<MessageFavorite>("owner_save_message_favorite", {
      owner,
      source,
      metadata,
    }),
  edit: (
    owner: string,
    id: string,
    expectedRevision: number,
    expectedDeviceId: string,
    metadata: FavoriteMetadata,
  ) =>
    invoke<MessageFavorite>("owner_edit_message_favorite", {
      owner,
      id,
      expectedRevision,
      expectedDeviceId,
      metadata,
    }),
  remove: (
    owner: string,
    id: string,
    expectedRevision: number,
    expectedDeviceId: string,
  ) =>
    invoke<void>("owner_remove_message_favorite", {
      owner,
      id,
      expectedRevision,
      expectedDeviceId,
    }),
};
