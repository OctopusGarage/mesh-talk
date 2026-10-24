import { invoke } from "@tauri-apps/api/core";
import type { MessageProtection } from "@/lib/types";
import type { DocumentVersion } from "./accountDocuments";

export type DraftVersion = DocumentVersion;
export interface DraftChange {
  text: string;
  protection?: MessageProtection;
  expected: DraftVersion | null;
}

export interface DraftSnapshot {
  session: string;
  drafts: Record<string, string>;
  protections?: Record<string, MessageProtection>;
  account_id?: string;
  versions?: Record<string, DraftVersion>;
  conflicts?: string[];
}
export const draftsApi = {
  load: (owner: string, expectedAccount?: string) =>
    invoke<DraftSnapshot>("load_conversation_drafts", {
      owner,
      expectedAccount,
    }),
  save: (
    owner: string,
    session: string,
    _drafts: Record<string, string>,
    _protections: Record<string, MessageProtection>,
    expectedAccount?: string,
    changes?: Record<string, DraftChange>,
  ) => {
    if (!expectedAccount || !changes)
      return Promise.reject(new Error("Draft account is not ready"));
    return invoke<DraftSnapshot | void>("save_conversation_drafts", {
      owner,
      session,
      expectedAccount,
      changes,
    });
  },
  publishLegacy: (owner: string, expectedAccount: string) =>
    invoke<{ imported: number; unmapped: number }>(
      "publish_legacy_conversation_drafts",
      { owner, expectedAccount },
    ),
};
