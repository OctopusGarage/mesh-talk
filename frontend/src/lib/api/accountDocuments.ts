import { invoke } from "@tauri-apps/api/core";

export type DocumentConversation =
  { kind: "account"; id: string } | { kind: "channel"; id: string };
export type AccountDocumentDomain =
  | "draft"
  | "read_receipt"
  | "customization_pack"
  | "avatar_override"
  | "dm_reaction"
  | "dm_recall";
export type AccountDocumentKey =
  | { kind: "draft"; conversation: DocumentConversation }
  | {
      kind: "read_receipt";
      conversation: DocumentConversation;
      message_id: string;
    }
  | { kind: "customization_pack"; pack_id: string }
  | { kind: "avatar_override"; target: DocumentConversation }
  | {
      kind: "dm_reaction";
      peer_account: string;
      target: string;
      actor_account: string;
      emoji: string;
    }
  | {
      kind: "dm_recall";
      peer_account: string;
      target: string;
      actor_account: string;
    };
export interface DocumentVersion {
  revision: number;
  writer_device: string;
  content_hash: number[];
}
export interface AccountDocumentMeta extends DocumentVersion {
  key: AccountDocumentKey;
  content_len: number;
  tombstone: boolean;
}
export interface AccountDocumentPage {
  items: AccountDocumentMeta[];
  next: AccountDocumentKey | null;
  digest: number[];
}
export function documentVersion(meta: AccountDocumentMeta): DocumentVersion {
  return {
    revision: meta.revision,
    writer_device: meta.writer_device,
    content_hash: meta.content_hash,
  };
}
export function sameDocumentVersion(a: DocumentVersion, b: DocumentVersion) {
  return (
    a.revision === b.revision &&
    a.writer_device === b.writer_device &&
    a.content_hash.length === b.content_hash.length &&
    a.content_hash.every((byte, index) => byte === b.content_hash[index])
  );
}
function encoded(bytes: Uint8Array) {
  if (bytes.byteLength > 12 * 1024 * 1024)
    throw new Error("Document exceeds the transfer limit");
  let binary = "";
  for (let index = 0; index < bytes.length; index += 8192)
    binary += String.fromCharCode(...bytes.subarray(index, index + 8192));
  return btoa(binary);
}
export const accountDocuments = {
  publishLegacyControls: (owner: string, expectedAccount: string) =>
    invoke<void>("publish_legacy_account_controls", { owner, expectedAccount }),
  list: (
    owner: string,
    expectedAccount: string,
    domain: AccountDocumentDomain,
    after: AccountDocumentKey | null = null,
    limit = 64,
  ) =>
    invoke<AccountDocumentPage>("get_account_documents", {
      owner,
      expectedAccount,
      domain,
      after,
      limit,
    }),
  meta: (owner: string, expectedAccount: string, key: AccountDocumentKey) =>
    invoke<AccountDocumentMeta | null>("get_account_document_meta", {
      owner,
      expectedAccount,
      key,
    }),
  read: async (
    owner: string,
    expectedAccount: string,
    key: AccountDocumentKey,
    expected: DocumentVersion,
  ) => {
    const bytes = await invoke<ArrayBuffer | number[]>(
      "read_account_document",
      { owner, expectedAccount, key, expected },
    );
    return Array.isArray(bytes) ? new Uint8Array(bytes).buffer : bytes;
  },
  put: (
    owner: string,
    expectedAccount: string,
    key: AccountDocumentKey,
    payload: Uint8Array,
    expected: DocumentVersion | null,
  ) =>
    invoke<AccountDocumentMeta>("put_account_document", {
      owner,
      expectedAccount,
      key,
      payloadBase64: encoded(payload),
      expected,
    }),
  remove: (
    owner: string,
    expectedAccount: string,
    key: AccountDocumentKey,
    expected: DocumentVersion | null,
  ) =>
    invoke<AccountDocumentMeta>("remove_account_document", {
      owner,
      expectedAccount,
      key,
      expected,
    }),
  markSeen: (
    owner: string,
    expectedAccount: string,
    conversation: DocumentConversation,
    messageId: string,
  ) =>
    invoke<AccountDocumentMeta>("mark_account_message_seen", {
      owner,
      expectedAccount,
      conversation,
      messageId,
    }),
};
